//! GIAP's last-mile veto over everything Goose sends to the model.
//!
//! Goose owns the reply loop, but between `override_system_prompt` and the
//! provider it still authors content of its own: an `# Additional
//! Instructions:` extras block (hints files, chat-mode text, recipe/final
//! -output extras), a `<turn-context>` block injected into the latest user
//! message on every call (time, working dir, turn budget), a silent
//! "created by Block" fallback when the override fails its minijinja
//! re-render, and conditional self-injected tools
//! (`platform__manage_schedule`, `recipe__final_output`).
//!
//! [`GiapProviderShim`] wraps the real provider at the one boundary where the
//! FINAL `(system, messages, tools)` is visible verbatim
//! (`Provider::stream`), and enforces GIAP ownership:
//!
//! - **System prompt**: when the incoming system derives from GIAP's static
//!   prefix (or is Goose's default/fallback prompt), it is rebuilt as exactly
//!   `prefix [+ GIAP's own extension appendix]` — every Goose-appended extra
//!   is dropped. Systems that do NOT derive from GIAP's prefix (Goose's
//!   auxiliary calls, e.g. compaction) pass through untouched so those flows
//!   keep working.
//! - **Messages**: `<turn-context>` blocks (Goose's per-turn MOIM injection)
//!   are stripped, recognised with Goose's own `is_turn_context_text`.
//! - **Tools**: when GIAP has published this turn's allow-set, tools not in
//!   it are vetoed — covering Goose's self-injected tools and anything else
//!   GIAP did not register.
//!
//! The shim is pure pass-through when no controls are set, so auxiliary
//! provider users (model listing, compaction) see no behaviour change.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use goose::conversation::message::Message;
use goose::providers::base::{MessageStream, Provider};
use goose_providers::errors::ProviderError;
use goose_providers::model::ModelConfig;
use rmcp::model::Tool;

/// Marker present in Goose's built-in `system.md` ("created by AAIF") and in
/// the hard fallback string ("created by Block") that replaces a failed
/// override render. If either ever reaches the provider on the chat path,
/// GIAP's prompt was lost and must be restored.
const GOOSE_DEFAULT_MARKER: &str = "general-purpose AI agent called goose";

/// How many Goose sessions keep live per-turn control state.
///
/// Goose exposes no session-end hook, so entries are evicted in insertion order
/// once the map exceeds this. Evicting a live session's entry only costs the
/// veto for that turn (the adapter republishes on every turn), so a generous
/// bound is enough — this exists to stop a long-lived server accumulating an
/// entry per session forever, not to be a tight cache.
const MAX_TRACKED_SESSIONS: usize = 64;

/// Per-turn control state for ONE Goose session.
///
/// Split out of [`ShimControls`] because tool selection (Phase D2) makes these
/// values differ between sessions. While every session got an identical set the
/// single global slot was benign; the moment sets diverge, a shared slot means
/// session A's provider call reads session B's allow-set.
#[derive(Default)]
pub struct SessionControls {
    /// Per-turn GIAP-owned appendix (prompt extras + skills), rebuilt fresh
    /// each turn. Kept separate from the prefix so the veto can truncate
    /// Goose's extras without losing GIAP's own.
    turn_appendix: Mutex<Option<String>>,
    /// Exact (prefixed) tool names allowed for this session. `None` disables
    /// tool filtering entirely.
    allowed_tools: Mutex<Option<HashSet<String>>>,
}

impl SessionControls {
    pub fn set_turn_appendix(&self, appendix: Option<String>) {
        *self.turn_appendix.lock().unwrap_or_else(|e| e.into_inner()) = appendix;
    }

    pub fn set_allowed_tools(&self, tools: HashSet<String>) {
        *self.allowed_tools.lock().unwrap_or_else(|e| e.into_inner()) = Some(tools);
    }

    /// Widen the allow-set in place (the `enable_tool_group` escape hatch).
    ///
    /// Takes effect on the next provider call — including the next call of the
    /// turn that triggered it, because Goose re-reads the provider each
    /// iteration and the shim filters on every call. A no-op when no allow-set
    /// is published (nothing is being filtered, so nothing needs widening).
    pub fn extend_allowed_tools<I: IntoIterator<Item = String>>(&self, tools: I) {
        let mut guard = self.allowed_tools.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = guard.as_mut() {
            set.extend(tools);
        }
    }

    pub fn allowed_tools_snapshot(&self) -> Option<HashSet<String>> {
        self.allowed_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Whether a tool call may proceed. `true` when no allow-set is published,
    /// matching the shim's pass-through semantics.
    pub fn is_tool_allowed(&self, tool: &str) -> bool {
        self.allowed_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_none_or(|set| set.contains(tool))
    }
}

/// Mutable controls shared between [`GooseAdapter`] and the shim.
///
/// The adapter writes these while assembling a chat turn; the shim reads them
/// inside `Provider::stream`, resolving the session via
/// [`goose::session_context::current_session_id`]. All `None` means "no
/// enforcement" (pass-through).
///
/// ## Global vs per-session
///
/// `system_prefix` and `extension_appendix` are deliberately GLOBAL. The prefix
/// IS the KV prompt prefix: it must be byte-identical across turns AND across
/// sessions, because `override_system_prompt` is agent-wide and
/// `last_prefix_hash` is a single slot — a per-session prefix would re-issue the
/// override on every session switch and destroy prefix reuse. The corollary is
/// that anything session-specific must ride the user message's
/// `<system-context>`, which is where the dormant-tool-group listing goes.
#[derive(Default)]
pub struct ShimControls {
    /// GIAP's authoritative static system prefix for the current chat model —
    /// the exact string passed to `override_system_prompt`.
    system_prefix: Mutex<Option<String>>,
    /// External MCP extension listing — recomputed only when the tool cache
    /// refreshes, so it persists across turns (same lifetime as the goose
    /// extra it mirrors).
    extension_appendix: Mutex<Option<String>>,
    /// Per-turn state keyed by GOOSE session id.
    sessions: RwLock<SessionMap>,
}

/// Session entries plus their insertion order, for bounded eviction.
#[derive(Default)]
struct SessionMap {
    entries: std::collections::HashMap<String, Arc<SessionControls>>,
    order: VecDeque<String>,
}

impl ShimControls {
    pub fn set_system_prefix(&self, prefix: String) {
        *self.system_prefix.lock().unwrap_or_else(|e| e.into_inner()) = Some(prefix);
    }

    pub fn set_extension_appendix(&self, appendix: Option<String>) {
        *self
            .extension_appendix
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = appendix;
    }

    /// The control entry for a Goose session, created on first use.
    ///
    /// Returned as an `Arc` so a caller (the chat stream's tool-call guard, the
    /// escape hatch) can hold it and observe live updates without re-locking the
    /// map.
    pub fn session(&self, goose_session_id: &str) -> Arc<SessionControls> {
        if let Some(existing) = self
            .sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .get(goose_session_id)
        {
            return existing.clone();
        }
        let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        // Re-check: another writer may have raced us between the two locks.
        if let Some(existing) = map.entries.get(goose_session_id) {
            return existing.clone();
        }
        let entry = Arc::new(SessionControls::default());
        map.entries
            .insert(goose_session_id.to_string(), entry.clone());
        map.order.push_back(goose_session_id.to_string());
        while map.order.len() > MAX_TRACKED_SESSIONS {
            if let Some(oldest) = map.order.pop_front() {
                map.entries.remove(&oldest);
            }
        }
        entry
    }

    /// The entry for a session, WITHOUT creating one. Used by the shim: an
    /// auxiliary provider call for a session GIAP never chatted in must stay
    /// pass-through rather than mint an empty entry.
    fn existing_session(&self, goose_session_id: &str) -> Option<Arc<SessionControls>> {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .get(goose_session_id)
            .cloned()
    }

    #[cfg(test)]
    fn tracked_sessions(&self) -> usize {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }
}

/// Provider decorator enforcing GIAP's veto. See module docs.
pub struct GiapProviderShim {
    inner: Arc<dyn Provider>,
    controls: Arc<ShimControls>,
}

impl GiapProviderShim {
    pub fn new(inner: Arc<dyn Provider>, controls: Arc<ShimControls>) -> Self {
        Self { inner, controls }
    }
}

/// The enforced system string for this call, or `None` to pass through
/// (auxiliary calls whose prompt GIAP does not own).
fn enforce_system(
    incoming: &str,
    prefix: &Option<String>,
    appendices: &[&Option<String>],
) -> Option<String> {
    let prefix = prefix.as_ref()?;
    let giap_owned = incoming.starts_with(prefix.as_str());
    let goose_default = incoming.contains(GOOSE_DEFAULT_MARKER);
    if !giap_owned && !goose_default {
        return None;
    }
    let mut rebuilt = prefix.clone();
    for apx in appendices.iter().filter_map(|a| a.as_deref()) {
        if !apx.is_empty() {
            rebuilt.push_str("\n\n");
            rebuilt.push_str(apx);
        }
    }
    if rebuilt == incoming {
        None // already exactly what GIAP intends — avoid an allocation swap
    } else {
        Some(rebuilt)
    }
}

/// Strip Goose's injected `<turn-context>` blocks from message content.
/// Returns `None` when nothing was stripped (no clone needed).
fn strip_turn_context(messages: &[Message]) -> Option<Vec<Message>> {
    let has_injection = messages.iter().any(|m| {
        m.content.iter().any(|c| {
            c.as_text()
                .is_some_and(goose::conversation::is_turn_context_text)
        })
    });
    if !has_injection {
        return None;
    }
    Some(
        messages
            .iter()
            .map(|m| {
                let mut m = m.clone();
                m.content.retain(|c| {
                    !c.as_text()
                        .is_some_and(goose::conversation::is_turn_context_text)
                });
                m
            })
            .filter(|m| !m.content.is_empty())
            .collect(),
    )
}

/// Provider names whose format layer already relocates tool-result images
/// correctly, so promotion must NOT run for them.
///
/// `formats/openai.rs`, `formats/google.rs` and `formats/databricks.rs` each
/// pull an image out of a tool response and re-host it as a following user
/// message. Promoting on top of that would send every camera frame twice.
fn provider_relocates_tool_images(provider_name: &str) -> bool {
    !matches!(provider_name, "local" | "gguf")
}

/// Lift images out of tool responses into a top-level user message (phase F3).
///
/// # Why this is here and not in the engine
///
/// A GIAP MCP tool CAN return `rmcp` image content — that part of the protocol
/// works. Two things in the goose local-inference engine stop it reaching the
/// model, and both are fork-side:
///
/// 1. `goose-local-inference/src/multimodal.rs` matches only a TOP-LEVEL
///    `MessageContent::Image`. `MessageContent::ToolResponse(_)` falls into its
///    catch-all arm, so images nested in a tool result are never extracted for
///    mtmd.
/// 2. `goose-local-inference/src/lib.rs` calls `strip_image_parts_from_messages`
///    unconditionally — with no vision guard — replacing the `image_url` part
///    that `formats/openai.rs` helpfully relocates with an apology string.
///
/// The shim is the one boundary that sees the final `(system, messages, tools)`
/// before the provider does, so promoting here puts the image exactly where the
/// engine's extractor looks, without touching the submodule. When the fork gains
/// a proper tool-result image path this function becomes a no-op and can go.
///
/// Returns `None` when there is nothing to promote, so the common case does not
/// clone the conversation.
fn promote_tool_result_images(messages: &[Message], max_images: usize) -> Option<Vec<Message>> {
    use goose::conversation::message::MessageContent;
    use rmcp::model::RawContent;

    // Newest-first: when a session has accumulated several looks at a camera,
    // the most recent frame is the one being asked about.
    let mut promoted: Vec<(String, String)> = Vec::new();
    'outer: for msg in messages.iter().rev() {
        for content in msg.content.iter().rev() {
            let MessageContent::ToolResponse(tr) = content else {
                continue;
            };
            let Ok(result) = &tr.tool_result else {
                continue;
            };
            for part in result.content.iter().rev() {
                if let RawContent::Image(img) = &part.raw {
                    promoted.push((img.data.clone(), img.mime_type.clone()));
                    if promoted.len() >= max_images {
                        break 'outer;
                    }
                }
            }
        }
    }
    if promoted.is_empty() {
        return None;
    }
    // Restore chronological order for the model.
    promoted.reverse();

    let mut out = messages.to_vec();
    // A dedicated trailing user message rather than editing an existing one:
    // rewriting a tool-response message would break the call/response pairing
    // every provider validates, and appending to the last user message would
    // reorder it after its own assistant reply.
    let mut carrier = Message::user().with_text(
        "The images below are the frames returned by the tool call above. Describe only what \
         you can actually see in them.",
    );
    for (data, mime) in &promoted {
        carrier = carrier.with_image(data, mime);
    }
    out.push(carrier);
    Some(out)
}

/// Retain only allow-listed tools. Returns `None` when nothing was vetoed.
fn enforce_tools(tools: &[Tool], allowed: &Option<HashSet<String>>) -> Option<Vec<Tool>> {
    let allowed = allowed.as_ref()?;
    if tools.iter().all(|t| allowed.contains(t.name.as_ref())) {
        return None;
    }
    Some(
        tools
            .iter()
            .filter(|t| allowed.contains(t.name.as_ref()))
            .cloned()
            .collect(),
    )
}

/// Strip mechanical schemars/serde boilerplate from a tool's input schema.
/// Every char here is re-prefilled by the local model on every turn:
/// - `"$schema"` draft URI and struct-name `"title"` — zero instruction value;
/// - integer-width artifacts: `"format": "uintN"/"intN"` plus the
///   `minimum: 0` / power-of-two `maximum` bounds pairs serde derives from
///   Rust integer types (a real, hand-written bound is kept).
///
/// Walks nested schema objects (`properties` values, `items`, `$defs`,
/// `anyOf`/`oneOf`/`allOf`) without ever touching `properties` KEYS, so a
/// parameter genuinely named "title" survives.
fn minify_schema_object(obj: &mut serde_json::Map<String, serde_json::Value>) {
    obj.remove("$schema");
    obj.remove("title");

    let int_width_artifact = obj
        .get("format")
        .and_then(|f| f.as_str())
        .is_some_and(|f| f.starts_with("uint") || f.starts_with("int"));
    if int_width_artifact {
        obj.remove("format");
        let min_is_zero = obj.get("minimum").and_then(|v| v.as_u64()) == Some(0);
        let max_is_width = matches!(
            obj.get("maximum").and_then(|v| v.as_u64()),
            Some(255) | Some(65535) | Some(4294967295)
        );
        if min_is_zero && max_is_width {
            obj.remove("minimum");
            obj.remove("maximum");
        }
    }

    for key in ["items", "additionalProperties"] {
        if let Some(serde_json::Value::Object(child)) = obj.get_mut(key) {
            minify_schema_object(child);
        }
    }
    for key in ["properties", "$defs", "definitions"] {
        if let Some(serde_json::Value::Object(children)) = obj.get_mut(key) {
            for child in children.values_mut() {
                if let serde_json::Value::Object(child) = child {
                    minify_schema_object(child);
                }
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(serde_json::Value::Array(variants)) = obj.get_mut(key) {
            for v in variants.iter_mut() {
                if let serde_json::Value::Object(child) = v {
                    minify_schema_object(child);
                }
            }
        }
    }
}

/// Minified copies of `tools`. Returns `None` when nothing changed.
fn minify_tools(tools: &[Tool]) -> Option<Vec<Tool>> {
    let mut changed = false;
    let minified: Vec<Tool> = tools
        .iter()
        .map(|t| {
            let mut schema = (*t.input_schema).clone();
            minify_schema_object(&mut schema);
            if schema != *t.input_schema {
                changed = true;
                let mut t = t.clone();
                t.input_schema = Arc::new(schema);
                t
            } else {
                t.clone()
            }
        })
        .collect();
    changed.then_some(minified)
}

#[async_trait]
impl Provider for GiapProviderShim {
    fn get_name(&self) -> &str {
        self.inner.get_name()
    }

    async fn stream(
        &self,
        model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        // Which session is this call for? Goose wraps every provider call in
        // `session_context::with_session_id` (reply_parts.rs) — the task-local it
        // already uses to stamp the `agent-session-id` header on provider HTTP
        // requests. That is the only session identity reaching `stream()`:
        // `Provider::stream` takes no session argument, and Goose holds ONE
        // agent-wide provider slot, so a shim INSTANCE per session would not
        // work either (session B's `update_provider` would hijack session A's
        // in-flight reply).
        //
        // `None` — an auxiliary call made outside the scope — stays pure
        // pass-through for the per-session controls, which is exactly right.
        let session = goose::session_context::current_session_id()
            .and_then(|sid| self.controls.existing_session(&sid));

        let (prefix, ext_apx) = {
            (
                self.controls
                    .system_prefix
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
                self.controls
                    .extension_appendix
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            )
        };
        let (turn_apx, allowed) = match &session {
            Some(s) => (
                s.turn_appendix
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
                s.allowed_tools_snapshot(),
            ),
            None => (None, None),
        };

        let enforced_system = enforce_system(system, &prefix, &[&turn_apx, &ext_apx]);
        let stripped_messages = strip_turn_context(messages);
        // Phase F3: run AFTER the turn-context strip so the promoted carrier is
        // built from the messages the provider will actually receive.
        let promoted_messages = if provider_relocates_tool_images(self.inner.get_name()) {
            None
        } else {
            promote_tool_result_images(
                stripped_messages.as_deref().unwrap_or(messages),
                pond_core::models::domain::image_limits::MAX_IMAGES_PER_TURN,
            )
        };
        let final_messages: &[Message] = promoted_messages
            .as_deref()
            .or(stripped_messages.as_deref())
            .unwrap_or(messages);
        let vetoed_tools = enforce_tools(tools, &allowed);
        // Minify AFTER the veto so we never pay for tools about to be dropped.
        let minified_tools = minify_tools(vetoed_tools.as_deref().unwrap_or(tools));
        let final_tools: &[Tool] = minified_tools
            .as_deref()
            .or(vetoed_tools.as_deref())
            .unwrap_or(tools);

        if enforced_system.is_some()
            || stripped_messages.is_some()
            || vetoed_tools.is_some()
            || promoted_messages.is_some()
        {
            tracing::debug!(
                system_rebuilt = enforced_system.is_some(),
                turn_context_stripped = stripped_messages.is_some(),
                tools_vetoed = vetoed_tools.is_some(),
                tool_images_promoted = promoted_messages.is_some(),
                "GIAP provider shim enforced ownership"
            );
        }

        // Prompt-cost accounting (debug only — serialization is skipped when
        // the level is off): chars/4 approximates tokens, making the split
        // between system prompt and tools JSON visible per call.
        if tracing::enabled!(tracing::Level::DEBUG) {
            let final_system = enforced_system.as_deref().unwrap_or(system);
            let tools_chars = serde_json::to_string(final_tools)
                .map(|s| s.len())
                .unwrap_or(0);
            tracing::debug!(
                system_chars = final_system.len(),
                // `tools_offered` is what Goose handed us (always the full
                // union); `tools_count` is what the model actually sees after
                // Phase D selection. The gap is the saving.
                tools_offered = tools.len(),
                tools_count = final_tools.len(),
                tools_json_chars = tools_chars,
                session_scoped = session.is_some(),
                "provider payload size"
            );
        }

        self.inner
            .stream(
                model_config,
                enforced_system.as_deref().unwrap_or(system),
                final_messages,
                final_tools,
            )
            .await
    }

    async fn get_context_limit(&self, model_config: &ModelConfig) -> Result<usize, ProviderError> {
        self.inner.get_context_limit(model_config).await
    }

    fn retry_config(&self) -> goose_providers::retry::RetryConfig {
        self.inner.retry_config()
    }

    async fn fetch_supported_models(&self) -> Result<Vec<String>, ProviderError> {
        self.inner.fetch_supported_models().await
    }

    async fn fetch_model_info(
        &self,
        model_name: &str,
    ) -> Result<goose::providers::base::ModelInfo, ProviderError> {
        self.inner.fetch_model_info(model_name).await
    }

    fn skip_canonical_filtering(&self) -> bool {
        self.inner.skip_canonical_filtering()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "<identity>\nYou are Goose, a home assistant.\n</identity>";

    fn some(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn exact_giap_system_passes_untouched() {
        assert_eq!(enforce_system(PREFIX, &some(PREFIX), &[&None, &None]), None);
    }

    #[test]
    fn goose_appended_extras_are_truncated() {
        let incoming = format!(
            "{PREFIX}\n\n# Additional Instructions:\n\n### Global Hints\nThese are my global goose hints."
        );
        assert_eq!(
            enforce_system(&incoming, &some(PREFIX), &[&None, &None]),
            Some(PREFIX.to_string())
        );
    }

    #[test]
    fn giap_appendices_survive_the_veto() {
        let incoming = format!("{PREFIX}\n\n# Additional Instructions:\n\nstuff goose added");
        let turn =
            some("<extension-notes name=\"skill:water\">\nwater the plants\n</extension-notes>");
        let ext = some("# MCP Extensions\n- music");
        assert_eq!(
            enforce_system(&incoming, &some(PREFIX), &[&turn, &ext]),
            Some(format!(
                "{PREFIX}\n\n{}\n\n{}",
                turn.as_deref().unwrap(),
                ext.as_deref().unwrap()
            ))
        );
    }

    #[test]
    fn goose_fallback_prompt_is_replaced() {
        let incoming = "You are a general-purpose AI agent called goose, created by Block";
        assert_eq!(
            enforce_system(incoming, &some(PREFIX), &[&None, &None]),
            Some(PREFIX.to_string())
        );
    }

    /// Auxiliary calls (compaction, etc.) use their own prompts — untouched.
    #[test]
    fn foreign_system_prompts_pass_through() {
        let incoming = "Summarise the conversation below into key points.";
        assert_eq!(
            enforce_system(incoming, &some(PREFIX), &[&None, &None]),
            None
        );
    }

    #[test]
    fn no_prefix_configured_means_pass_through() {
        assert_eq!(enforce_system("anything", &None, &[&None, &None]), None);
    }

    #[test]
    fn turn_context_blocks_are_stripped_from_messages() {
        let turn_ctx = "<turn-context>\n<current-time>2026-07-26 14:00</current-time>\n<working-directory>/home</working-directory>\n</turn-context>";
        assert!(goose::conversation::is_turn_context_text(turn_ctx));
        let msg = Message::user()
            .with_text("real question")
            .with_text(turn_ctx);
        let stripped = strip_turn_context(&[msg]).expect("injection present");
        assert_eq!(stripped.len(), 1);
        assert_eq!(stripped[0].content.len(), 1);
        assert_eq!(stripped[0].content[0].as_text(), Some("real question"));
    }

    #[test]
    fn clean_messages_are_not_cloned() {
        let msg = Message::user().with_text("hello");
        assert!(strip_turn_context(&[msg]).is_none());
    }

    // ── F3: tool-result image promotion ──────────────────────────────────

    fn image_tool_response(id: &str, note: &str, images: &[(&str, &str)]) -> Message {
        let mut parts = vec![rmcp::model::Content::text(note.to_string())];
        for (data, mime) in images {
            parts.push(rmcp::model::Content::image(
                data.to_string(),
                mime.to_string(),
            ));
        }
        Message::user().with_tool_response(id, Ok(rmcp::model::CallToolResult::success(parts)))
    }

    fn top_level_images(msg: &Message) -> Vec<(String, String)> {
        msg.content
            .iter()
            .filter_map(|c| match c {
                goose::conversation::message::MessageContent::Image(i) => {
                    Some((i.data.clone(), i.mime_type.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_text_only_conversation_is_not_cloned() {
        let msgs = vec![
            Message::user().with_text("hi"),
            tool_text_response("call-1", "the door is locked"),
        ];
        assert!(promote_tool_result_images(&msgs, 4).is_none());
    }

    fn tool_text_response(id: &str, body: &str) -> Message {
        Message::user().with_tool_response(
            id,
            Ok(rmcp::model::CallToolResult::success(vec![
                rmcp::model::Content::text(body.to_string()),
            ])),
        )
    }

    #[test]
    fn a_tool_result_image_is_promoted_to_a_trailing_user_message() {
        let msgs = vec![
            Message::user().with_text("what is at the door?"),
            image_tool_response("call-1", "front-door frame", &[("AAAA", "image/jpeg")]),
        ];
        let out = promote_tool_result_images(&msgs, 4).expect("an image was returned");
        assert_eq!(out.len(), 3, "the original messages are kept intact");
        // The tool response itself is untouched — rewriting it would break the
        // call/response pairing providers validate.
        assert_eq!(out[1].content.len(), msgs[1].content.len());
        let carrier = out.last().unwrap();
        assert_eq!(
            top_level_images(carrier),
            vec![("AAAA".to_string(), "image/jpeg".to_string())]
        );
    }

    #[test]
    fn several_frames_keep_chronological_order() {
        let msgs = vec![
            image_tool_response("call-1", "older", &[("AAAA", "image/jpeg")]),
            image_tool_response("call-2", "newer", &[("BBBB", "image/jpeg")]),
        ];
        let out = promote_tool_result_images(&msgs, 4).unwrap();
        assert_eq!(
            top_level_images(out.last().unwrap())
                .into_iter()
                .map(|(d, _)| d)
                .collect::<Vec<_>>(),
            vec!["AAAA".to_string(), "BBBB".to_string()]
        );
    }

    /// The cap is the same one a manual attachment obeys — a camera window that
    /// returned six frames must not become a six-image prefill.
    #[test]
    fn promotion_is_capped_and_keeps_the_newest_frames() {
        let msgs = vec![image_tool_response(
            "call-1",
            "window",
            &[
                ("F1", "image/jpeg"),
                ("F2", "image/jpeg"),
                ("F3", "image/jpeg"),
                ("F4", "image/jpeg"),
                ("F5", "image/jpeg"),
                ("F6", "image/jpeg"),
            ],
        )];
        let out = promote_tool_result_images(&msgs, 2).unwrap();
        let kept: Vec<String> = top_level_images(out.last().unwrap())
            .into_iter()
            .map(|(d, _)| d)
            .collect();
        assert_eq!(kept, vec!["F5".to_string(), "F6".to_string()]);
    }

    /// HTTP formats already relocate tool-result images; promoting on top of
    /// that would send every frame twice.
    #[test]
    fn only_the_local_engine_needs_promotion() {
        assert!(!provider_relocates_tool_images("local"));
        assert!(!provider_relocates_tool_images("gguf"));
        for http in ["openai", "anthropic", "google", "databricks", "ollama"] {
            assert!(provider_relocates_tool_images(http), "{http}");
        }
    }

    fn tool(name: &str) -> Tool {
        Tool::new(
            name.to_string(),
            "desc".to_string(),
            rmcp::object!({"type": "object"}),
        )
    }

    #[test]
    fn tools_outside_the_allowlist_are_vetoed() {
        let mine = tool("giap-weather__get_current_weather");
        let goose_tool = tool("platform__manage_schedule");
        let allowed: HashSet<String> =
            std::iter::once("giap-weather__get_current_weather".to_string()).collect();
        let out = enforce_tools(&[mine.clone(), goose_tool], &Some(allowed)).expect("veto fired");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, mine.name);
    }

    #[test]
    fn allowlisted_tools_pass_without_clone() {
        let mine = tool("giap-weather__get_current_weather");
        let allowed: HashSet<String> =
            std::iter::once("giap-weather__get_current_weather".to_string()).collect();
        assert!(enforce_tools(&[mine], &Some(allowed)).is_none());
    }

    #[test]
    fn no_allowlist_means_no_tool_filtering() {
        assert!(enforce_tools(&[tool("platform__manage_schedule")], &None).is_none());
    }

    #[test]
    fn minifier_strips_boilerplate_and_int_width_noise() {
        let schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "ForecastParams",
            "type": "object",
            "properties": {
                "days": {
                    "description": "1-7, default 3.",
                    "type": ["integer", "null"],
                    "format": "uint8",
                    "maximum": 255,
                    "minimum": 0
                },
                "location": { "type": ["string", "null"] }
            }
        });
        let mut obj = schema.as_object().unwrap().clone();
        minify_schema_object(&mut obj);
        let out = serde_json::Value::Object(obj);
        assert!(out.get("$schema").is_none());
        assert!(out.get("title").is_none());
        let days = &out["properties"]["days"];
        assert!(days.get("format").is_none());
        assert!(days.get("maximum").is_none());
        assert!(days.get("minimum").is_none());
        assert_eq!(days["description"], "1-7, default 3.");
        assert_eq!(out["properties"]["location"]["type"][0], "string");
    }

    #[test]
    fn minifier_keeps_real_bounds_and_title_named_params() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                // A parameter genuinely named "title" must survive.
                "title": { "type": "string" },
                // Hand-written bounds (not an integer-width artifact pair).
                "limit": { "type": "integer", "format": "uint8", "minimum": 1, "maximum": 10 }
            }
        });
        let mut obj = schema.as_object().unwrap().clone();
        minify_schema_object(&mut obj);
        let out = serde_json::Value::Object(obj);
        assert!(out["properties"].get("title").is_some());
        assert_eq!(out["properties"]["limit"]["minimum"], 1);
        assert_eq!(out["properties"]["limit"]["maximum"], 10);
        // The width-format marker itself still goes — it carries no meaning.
        assert!(out["properties"]["limit"].get("format").is_none());
    }

    #[test]
    fn minify_tools_returns_none_when_already_clean() {
        let clean = tool("giap-weather__get_current_weather");
        assert!(minify_tools(&[clean]).is_none());
    }

    // ── D1: session-keyed controls ─────────────────────────────────────────

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// The reason D1 exists. Before keying, two sessions shared one allow-set
    /// slot; with different tool selections that is a cross-session data race
    /// where one session's provider call filters by another's set.
    #[test]
    fn two_sessions_do_not_see_each_others_allow_sets() {
        let controls = ShimControls::default();
        let a = controls.session("goose-a");
        let b = controls.session("goose-b");

        a.set_allowed_tools(set(&["giap-weather__get_forecast"]));
        b.set_allowed_tools(set(&["giap-schedule__create_schedule"]));

        assert!(a.is_tool_allowed("giap-weather__get_forecast"));
        assert!(!a.is_tool_allowed("giap-schedule__create_schedule"));
        assert!(b.is_tool_allowed("giap-schedule__create_schedule"));
        assert!(!b.is_tool_allowed("giap-weather__get_forecast"));

        // And the veto itself filters per session, not globally.
        let tools = [
            tool("giap-weather__get_forecast"),
            tool("giap-schedule__create_schedule"),
        ];
        let for_a = enforce_tools(&tools, &a.allowed_tools_snapshot()).expect("veto fired");
        assert_eq!(for_a.len(), 1);
        assert_eq!(for_a[0].name.as_ref(), "giap-weather__get_forecast");
        let for_b = enforce_tools(&tools, &b.allowed_tools_snapshot()).expect("veto fired");
        assert_eq!(for_b[0].name.as_ref(), "giap-schedule__create_schedule");
    }

    #[test]
    fn turn_appendices_are_also_per_session() {
        let controls = ShimControls::default();
        let a = controls.session("goose-a");
        let b = controls.session("goose-b");
        a.set_turn_appendix(some("A's skills"));
        b.set_turn_appendix(some("B's skills"));
        assert_eq!(
            *a.turn_appendix.lock().unwrap(),
            Some("A's skills".to_string())
        );
        assert_eq!(
            *b.turn_appendix.lock().unwrap(),
            Some("B's skills".to_string())
        );
    }

    #[test]
    fn the_same_session_id_returns_the_same_live_entry() {
        let controls = ShimControls::default();
        let first = controls.session("goose-a");
        first.set_allowed_tools(set(&["giap-memory__recall_memories"]));
        let second = controls.session("goose-a");
        assert!(Arc::ptr_eq(&first, &second));
        // A widen through one handle is visible through the other — this is what
        // lets the escape hatch affect the in-flight turn.
        second.extend_allowed_tools(["giap-vision__list_camera_events".to_string()]);
        assert!(first.is_tool_allowed("giap-vision__list_camera_events"));
    }

    /// D2 escape hatch: enabling a group widens the live allow-set.
    #[test]
    fn extending_the_allow_set_admits_newly_enabled_tools() {
        let controls = ShimControls::default();
        let s = controls.session("goose-a");
        s.set_allowed_tools(set(&["giap-toolkit__enable_tool_group"]));
        assert!(!s.is_tool_allowed("giap-schedule__create_schedule"));

        s.extend_allowed_tools([
            "giap-schedule__create_schedule".to_string(),
            "giap-schedule__list_schedules".to_string(),
        ]);

        assert!(s.is_tool_allowed("giap-schedule__create_schedule"));
        assert!(s.is_tool_allowed("giap-schedule__list_schedules"));
        // The original core tool is not lost in the widen.
        assert!(s.is_tool_allowed("giap-toolkit__enable_tool_group"));
    }

    /// Widening when nothing is being filtered must not accidentally START
    /// filtering — that would narrow the surface, the opposite of the intent.
    #[test]
    fn extending_without_an_allow_set_stays_pass_through() {
        let controls = ShimControls::default();
        let s = controls.session("goose-a");
        s.extend_allowed_tools(["giap-schedule__create_schedule".to_string()]);
        assert!(s.allowed_tools_snapshot().is_none());
        assert!(s.is_tool_allowed("literally-anything"));
    }

    /// An auxiliary provider call for a session GIAP never chatted in must not
    /// mint an entry — otherwise the map grows on compaction traffic.
    #[test]
    fn lookup_without_creation_does_not_track_the_session() {
        let controls = ShimControls::default();
        assert!(controls.existing_session("never-seen").is_none());
        assert_eq!(controls.tracked_sessions(), 0);
        controls.session("real");
        assert!(controls.existing_session("real").is_some());
        assert_eq!(controls.tracked_sessions(), 1);
    }

    /// Goose gives us no session-end hook, so the map is bounded.
    #[test]
    fn session_tracking_is_bounded() {
        let controls = ShimControls::default();
        for i in 0..(MAX_TRACKED_SESSIONS + 25) {
            controls.session(&format!("goose-{i}"));
        }
        assert_eq!(controls.tracked_sessions(), MAX_TRACKED_SESSIONS);
        // Oldest evicted, newest retained.
        assert!(controls.existing_session("goose-0").is_none());
        assert!(controls
            .existing_session(&format!("goose-{}", MAX_TRACKED_SESSIONS + 24))
            .is_some());
    }
}
