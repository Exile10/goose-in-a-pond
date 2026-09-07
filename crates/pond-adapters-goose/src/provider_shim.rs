//! GIAP's last-mile veto over everything Goose sends to the model. [`GiapProviderShim`] wraps the
//! real provider at `Provider::stream`, the one boundary where the FINAL `(system, messages,
//! tools)` is visible: a system derived from GIAP's prefix is rebuilt as prefix plus appendix,
//! `<turn-context>` is stripped, tools outside the allow-set are vetoed; else pure pass-through.

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

/// How many Goose sessions keep live per-turn control state. Goose has no session-end hook, so
/// entries are evicted in insertion order past this bound. Evicting a live session only costs
/// the veto for one turn (the adapter republishes every turn), so the bound is generous.
const MAX_TRACKED_SESSIONS: usize = 64;

/// Per-turn control state for ONE Goose session. Split out of [`ShimControls`] because tool
/// selection (Phase D2) makes these values differ between sessions; a shared slot would let
/// session A's provider call read session B's allow-set.
#[derive(Default)]
pub struct SessionControls {
    /// Per-turn GIAP-owned appendix (prompt extras + skills), rebuilt fresh
    /// each turn. Kept separate from the prefix so the veto can truncate
    /// Goose's extras without losing GIAP's own.
    turn_appendix: Mutex<Option<String>>,
    /// Exact (prefixed) tool names allowed for this session. `None` disables
    /// tool filtering entirely.
    allowed_tools: Mutex<Option<HashSet<String>>>,
    /// The complete system prompt this session owns, bypassing the prefix-plus-appendices
    /// rebuild. PAI-6 P3, SUBAGENT sessions only: a child's prompt starts with the parent's
    /// static prefix, so the rebuild would silently replace its delegation envelope (turn budget,
    /// no-delegation rule, exact tool names) with the GLOBAL extension appendix and warn nothing.
    system_override: Mutex<Option<String>>,
}

impl SessionControls {
    pub fn set_turn_appendix(&self, appendix: Option<String>) {
        *self.turn_appendix.lock().unwrap_or_else(|e| e.into_inner()) = appendix;
    }

    pub fn set_allowed_tools(&self, tools: HashSet<String>) {
        *self.allowed_tools.lock().unwrap_or_else(|e| e.into_inner()) = Some(tools);
    }

    /// Widen the allow-set in place (the `enable_tool_group` escape hatch). Takes effect on the
    /// next provider call, including later calls of the same turn, since the shim filters on
    /// every call. A no-op when no allow-set is published (nothing is being filtered).
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

    /// Declare that this session's system prompt is owned wholesale, so the
    /// shim must deliver it verbatim rather than rebuilding it.
    pub fn set_system_override(&self, system: Option<String>) {
        *self
            .system_override
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = system;
    }

    pub fn system_override_snapshot(&self) -> Option<String> {
        self.system_override
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

/// Mutable controls shared between [`GooseAdapter`] (writer, while assembling a turn) and the
/// shim (reader, inside `Provider::stream`, session from `current_session_id`); all `None` means
/// pass-through. `system_prefix` and `extension_appendix` are GLOBAL: the prefix IS the KV prefix
/// and a per-session one would churn `last_prefix_hash`; session text rides `<system-context>`.
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

    /// The control entry for a Goose session, created on first use. Returned as an `Arc` so a
    /// caller (the chat stream's tool-call guard, the escape hatch) can observe live updates
    /// without re-locking the map.
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

    /// Drop a session's entry outright. PAI-6 P3: every subagent run mints an entry and the map
    /// evicts OLDEST-FIRST regardless of liveness, so a stream of delegations would evict a
    /// long-running parent's allow-set and make its turns pass-through (taking a Guest's
    /// `subtract_guest_denied_tools` result with them). Release a child the moment its run ends.
    pub fn forget_session(&self, goose_session_id: &str) {
        let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        if map.entries.remove(goose_session_id).is_some() {
            map.order.retain(|id| id != goose_session_id);
        }
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

/// The last minification, kept so an unchanged tool set is not re-minified on every call.
/// `input` is retained deliberately: identity is compared by `Arc::ptr_eq` on `input_schema`,
/// which is only sound while the compared allocations stay alive. Holding them prevents address
/// reuse by a different schema. The clone is a refcount bump per tool, not a schema copy.
struct MinifyCache {
    input: Vec<Tool>,
    output: Option<Vec<Tool>>,
}

impl MinifyCache {
    /// Whether `tools` is the same set, tool for tool, that produced `output`. Conservative:
    /// structurally identical schemas behind different allocations miss and re-minify. A false
    /// miss costs one minification; a false hit would send the model the wrong tool set.
    fn matches(&self, tools: &[Tool]) -> bool {
        self.input.len() == tools.len()
            && self
                .input
                .iter()
                .zip(tools)
                .all(|(a, b)| a.name == b.name && Arc::ptr_eq(&a.input_schema, &b.input_schema))
    }
}

/// Provider decorator enforcing GIAP's veto. See module docs.
pub struct GiapProviderShim {
    inner: Arc<dyn Provider>,
    controls: Arc<ShimControls>,
    /// Guarded by a plain `Mutex` rather than an async one: the critical section
    /// is a pointer comparison and a `Vec` clone, and it never awaits.
    minify_cache: Mutex<Option<MinifyCache>>,
    /// Where the wrapped provider sends its HTTP, when it sends any. The inner provider is Goose
    /// submodule code with its own reqwest client, invisible to the workspace egress guard, so
    /// the PAI-2 gate lives here at the one chokepoint. `None` means in-process inference, nothing
    /// gated. A constructor parameter, not a setter, so every construction site must answer it.
    endpoint: Option<String>,
}

impl GiapProviderShim {
    pub fn new(
        inner: Arc<dyn Provider>,
        controls: Arc<ShimControls>,
        endpoint: Option<String>,
    ) -> Self {
        Self {
            inner,
            controls,
            minify_cache: Mutex::new(None),
            endpoint,
        }
    }

    /// Gate one outbound provider call, PAI-2 style: refused before a packet leaves, recorded
    /// after. The denial maps to [`ProviderError::RequestFailed`] deliberately: `NetworkError` is
    /// in goose's retryable class, and retrying a policy refusal with backoff would turn a clear
    /// "offline mode refused this host" into thirty seconds of apparent hang.
    fn begin_egress(
        &self,
    ) -> Result<Option<pond_core::shared::services::egress::EgressCall>, ProviderError> {
        match &self.endpoint {
            None => Ok(None),
            Some(url) => match pond_core::shared::services::egress::begin(url, "LLM") {
                Ok(call) => Ok(Some(call)),
                Err(denied) => Err(ProviderError::RequestFailed(format!(
                    "GIAP refused this model call before it left the machine: {denied}"
                ))),
            },
        }
    }

    /// `minify_tools`, memoised on the tool set it was last given. The uncached call deep-clones
    /// and walks every schema on EVERY provider call (about a thousand small allocations) for an
    /// answer that changes only when an extension or allow-set does. The allow-set is NOT part of
    /// the key: this runs on `enforce_tools` output, so a narrowed set already misses on length.
    fn minify_tools_cached(&self, tools: &[Tool]) -> Option<Vec<Tool>> {
        let mut cache = self.minify_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.as_ref().filter(|c| c.matches(tools)) {
            return hit.output.clone();
        }
        let output = minify_tools(tools);
        *cache = Some(MinifyCache {
            input: tools.to_vec(),
            output: output.clone(),
        });
        output
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
    // Recognise OUR prompt ignoring trailing whitespace. GIAP's static prefix ends
    // `</output-quality>\n\n\n\n` and goose appends its block after `</output-quality>\n\n`, so
    // a plain `starts_with(prefix)` diverges inside GIAP's own newlines, calls the prompt somebody
    // else's, and passes ~28 KB of "# Additional Instructions" through with no warning fired.
    let anchor = prefix.trim_end();
    let giap_owned = !anchor.is_empty() && incoming.starts_with(anchor);
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

/// Provider names whose format layer already relocates tool-result images, so promotion must
/// NOT run for them: `formats/openai.rs`, `formats/google.rs` and `formats/databricks.rs` each
/// re-host a tool-response image as a following user message, so promoting would send it twice.
fn provider_relocates_tool_images(provider_name: &str) -> bool {
    !matches!(provider_name, "local" | "gguf")
}

/// Lift images out of tool responses into a top-level user message (phase F3). Two fork-side gaps
/// in the goose local-inference engine stop nested images reaching mtmd: `multimodal.rs` extracts
/// only top-level `MessageContent::Image`, and `lib.rs` strips `image_url` parts with no vision
/// guard. A no-op once the fork handles tool-result images; `None` means nothing to promote.
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

/// Strip mechanical schemars/serde boilerplate from a tool's input schema, since every char is
/// re-prefilled by the local model each turn: the `"$schema"` URI, the struct-name `"title"`, and
/// integer-width artifacts (`"format": "uintN"/"intN"` with the `minimum: 0` / power-of-two
/// `maximum` pair serde derives). Walks nested objects but never touches `properties` KEYS.
/// Write the final `(system, messages, tools)` as an OpenAI chat body when
/// `GIAP_CAPTURE_PAYLOAD` names a directory.
///
/// Exists so a candidate engine in an inference bake-off is measured on the prompt GIAP
/// actually sends. Reusing goose's `create_request` rather than hand-rolling the body keeps
/// the capture honest: the same serializer the HTTP providers use, so a replay differs from
/// a live call only by transport.
///
/// Failures are logged and swallowed -- a diagnostic must never fail a turn.
fn capture_payload(model_config: &ModelConfig, system: &str, messages: &[Message], tools: &[Tool]) {
    let Some(dir) = std::env::var_os("GIAP_CAPTURE_PAYLOAD") else {
        return;
    };
    write_payload_capture(
        std::path::Path::new(&dir),
        model_config,
        system,
        messages,
        tools,
    );
}

/// The capture itself, with the directory passed in.
///
/// Split from [`capture_payload`] so a test can exercise it without `set_var`: the env is
/// process-global and a test binary is threaded, so a test that set it would decide whether
/// OTHER tests capture.
fn write_payload_capture(
    dir: &std::path::Path,
    model_config: &ModelConfig,
    system: &str,
    messages: &[Message],
    tools: &[Tool],
) -> Option<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    let body = match goose_providers::formats::openai::create_request(
        model_config,
        system,
        messages,
        tools,
        &goose_providers::images::ImageFormat::OpenAi,
        true,
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("payload capture: could not build request body: {e}");
            return None;
        }
    };
    let Ok(text) = serde_json::to_string_pretty(&body) else {
        tracing::warn!("payload capture: body is not serializable");
        return None;
    };

    // The hash is over the body, so two turns that produce a byte-identical prompt land on
    // the same name -- which is how the capture proves prefix stability rather than assuming it.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!(
        "payload-{seq:04}-{}t-{:016x}.json",
        tools.len(),
        h.finish()
    ));

    if let Err(e) = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, &text)) {
        tracing::warn!("payload capture: could not write {}: {e}", path.display());
        return None;
    }
    tracing::info!(
        target: "giap::trace",
        kind = "payload_captured",
        path = %path.display(),
        tools = tools.len(),
        system_chars = system.len(),
        messages = messages.len(),
        "captured the final provider payload"
    );
    Some(path)
}

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
        // Before any payload work: a call the gate refuses should cost nothing,
        // and a refused call must not reach the veto/minify machinery below —
        // its output would describe a request that never happens.
        let egress = self.begin_egress()?;

        // Which session is this call for? Goose wraps every provider call in
        // `session_context::with_session_id` (reply_parts.rs), the only session identity that
        // reaches `stream()`; Goose holds ONE agent-wide provider slot, so a shim per session
        // cannot work. `None` (an auxiliary call outside the scope) stays pass-through.
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
        let (turn_apx, allowed, owned_system) = match &session {
            Some(s) => (
                s.turn_appendix
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
                s.allowed_tools_snapshot(),
                s.system_override_snapshot(),
            ),
            None => (None, None, None),
        };

        // PAI-6 P3. A session that owns its whole system prompt (a subagent) skips the
        // prefix-plus-appendices rebuild: a child's prompt starts with the same GIAP prefix as
        // the parent's, so the rebuild would silently swap its delegation envelope for the GLOBAL
        // extension appendix, which describes tools the child does not have.
        let enforced_system = match &owned_system {
            Some(owned) => (owned.as_str() != system).then(|| owned.clone()),
            None => enforce_system(system, &prefix, &[&turn_apx, &ext_apx]),
        };

        // The shim is the only thing that delivers GIAP's appendix, so a pass-through here is
        // load-bearing: the incoming system did not start with GIAP's prefix and the appendix is
        // attached to nothing. Silent for auxiliary calls with no session appendix; a warning when
        // a real per-turn appendix is dropped, e.g. a name containing `{{` that minijinja mangles.
        if enforced_system.is_none() && turn_apx.is_some() && owned_system.is_none() {
            tracing::warn!(
                target: "giap::trace",
                kind = "system_appendix_dropped",
                "this turn's prompt extras and skills were not delivered: the system prompt \
                 did not match GIAP's prefix, so the shim passed it through unchanged"
            );
        }
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
        let minified_tools = self.minify_tools_cached(vetoed_tools.as_deref().unwrap_or(tools));
        let selected: &[Tool] = minified_tools
            .as_deref()
            .or(vetoed_tools.as_deref())
            .unwrap_or(tools);
        // Order the tools so a KV prefix can survive a changed selection: the template renders
        // schemas in the order given, so two turns share a prefix only up to their first differing
        // tool. Sorting core-first makes the always-loaded groups a common prefix (measured 70% vs
        // 85% shared preamble against the real Gemma template). See `tool_group::prefix_sort_key`.
        let ordered_tools = {
            let mut v = selected.to_vec();
            v.sort_by(|a, b| {
                pond_core::mcp::domain::tool_group::prefix_sort_key(a.name.as_ref()).cmp(
                    &pond_core::mcp::domain::tool_group::prefix_sort_key(b.name.as_ref()),
                )
            });
            (v != selected).then_some(v)
        };
        let final_tools: &[Tool] = ordered_tools.as_deref().unwrap_or(selected);

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
            // WHY the veto did or did not fire, which the size alone cannot say:
            // `system_rebuilt = false` covers both "already exactly GIAP's" and "not recognised,
            // passed through untouched". The second is a silent hole in the ownership guarantee,
            // and telling them apart needs the prefix and the incoming head side by side.
            let prefix_snapshot = self
                .controls
                .system_prefix
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            tracing::trace!(
                incoming_chars = system.len(),
                final_chars = final_system.len(),
                giap_prefix_chars = prefix_snapshot.as_ref().map(String::len),
                starts_with_giap_prefix = prefix_snapshot
                    .as_deref()
                    .map(|p| system.starts_with(p)),
                // `?` not `%`: these are multi-line prompts, and a raw newline
                // ends the log line mid-field — which is how the first capture
                // of this came back with the one value that mattered missing.
                incoming_head = ?system.chars().take(160).collect::<String>(),
                giap_prefix_head = ?prefix_snapshot
                    .as_deref()
                    .map(|p| p.chars().take(160).collect::<String>())
                    .unwrap_or_default(),
                first_divergence = ?prefix_snapshot.as_deref().map(|p| {
                    p.chars()
                        .zip(system.chars())
                        .position(|(a, b)| a != b)
                        .unwrap_or(p.chars().count())
                }),
                // The window either side of the divergence, which is what says
                // whether goose APPENDED to GIAP's prompt (harmless, and what
                // `starts_with` assumes) or INSERTED into it (fatal to the
                // check, and invisible without this).
                prefix_at_divergence = ?prefix_snapshot.as_deref().map(|p| {
                    let d = p.chars().zip(system.chars()).position(|(a, b)| a != b).unwrap_or(0);
                    p.chars().skip(d.saturating_sub(60)).take(140).collect::<String>()
                }),
                incoming_at_divergence = ?prefix_snapshot.as_deref().map(|p| {
                    let d = p.chars().zip(system.chars()).position(|(a, b)| a != b).unwrap_or(0);
                    system.chars().skip(d.saturating_sub(60)).take(140).collect::<String>()
                }),
                "provider system prompt provenance"
            );
        }

        // Bake-off capture (`GIAP_CAPTURE_PAYLOAD=<dir>`). This is the only place the
        // FINAL payload exists: goose's own `sessions.db` stores the raw messages, not the
        // shim-enforced system prompt, the vetoed/minified tool array, or the core-first
        // ordering -- so an engine replayed from that store is not answering GIAP's prompt.
        // No-op, and no serialization cost, when the variable is unset.
        capture_payload(
            model_config,
            enforced_system.as_deref().unwrap_or(system),
            final_messages,
            final_tools,
        );

        let result = self
            .inner
            .stream(
                model_config,
                enforced_system.as_deref().unwrap_or(system),
                final_messages,
                final_tools,
            )
            .await;
        if let Some(call) = egress {
            // The provider abstracts the wire, so the real HTTP status is not
            // visible here; 200/500 is the same synthesis the IMAP adapter
            // records for its raw-TLS session. Ok means the stream was
            // ESTABLISHED — latency is time-to-stream, not time-to-last-token.
            call.finish(Some(if result.is_ok() { 200 } else { 500 }));
        }
        result
    }

    async fn get_context_limit(&self, model_config: &ModelConfig) -> Result<usize, ProviderError> {
        self.inner.get_context_limit(model_config).await
    }

    fn retry_config(&self) -> goose_providers::retry::RetryConfig {
        self.inner.retry_config()
    }

    async fn fetch_supported_models(&self) -> Result<Vec<String>, ProviderError> {
        // Model listing reaches the same host the chat does; an offline pond
        // has no business pinging a remote registry either.
        let egress = self.begin_egress()?;
        let result = self.inner.fetch_supported_models().await;
        if let Some(call) = egress {
            call.finish(Some(if result.is_ok() { 200 } else { 500 }));
        }
        result
    }

    async fn fetch_model_info(
        &self,
        model_name: &str,
    ) -> Result<goose::providers::base::ModelInfo, ProviderError> {
        let egress = self.begin_egress()?;
        let result = self.inner.fetch_model_info(model_name).await;
        if let Some(call) = egress {
            call.finish(Some(if result.is_ok() { 200 } else { 500 }));
        }
        result
    }

    fn skip_canonical_filtering(&self) -> bool {
        self.inner.skip_canonical_filtering()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The veto declining by four newlines: GIAP's static prefix ends `</output-quality>\n\n\n\n`
    /// and goose appends its hints block after `</output-quality>\n\n`, so the two diverge INSIDE
    /// GIAP's own trailing whitespace. `starts_with` said "not ours" and ~28 KB of developer-agent
    /// hints reached a household assistant, logged just like the healthy "already correct" case.
    #[test]
    fn goose_extras_are_vetoed_even_when_our_prefix_ends_in_blank_lines() {
        let prefix = "<identity>\nYou are Goose.\n</output-quality>\n\n\n\n".to_string();
        let incoming = "<identity>\nYou are Goose.\n</output-quality>\n\n\
# Additional Instructions:\n\n### Project Hints\nhints here";

        let out = enforce_system(incoming, &Some(prefix.clone()), &[]);

        assert_eq!(
            out.as_deref(),
            Some(prefix.as_str()),
            "goose's appended block must be vetoed, not passed through"
        );
    }

    /// The healthy case must keep answering `None`, or every turn pays an
    /// allocation swap to rewrite a prompt that was already right.
    #[test]
    fn an_already_correct_prompt_is_still_left_alone() {
        let prefix = "<identity>\nYou are Goose.\n".to_string();
        assert_eq!(enforce_system(&prefix, &Some(prefix.clone()), &[]), None);
    }

    /// Somebody else's prompt is still not ours to rewrite. Trimming the anchor
    /// must not widen recognition to prompts that share no prefix at all.
    #[test]
    fn a_foreign_prompt_is_still_passed_through() {
        let prefix = "<identity>\nYou are Goose.\n\n\n".to_string();
        assert_eq!(
            enforce_system("Something else entirely.", &Some(prefix), &[]),
            None
        );
    }

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

    /// The bake-off replays these files against candidate engines, so the capture has to be
    /// the payload GIAP sends -- and it has to be BYTE-STABLE across two identical turns, or
    /// a "prefix moved" finding would just be capture noise. Same body, same name, one file.
    #[test]
    fn an_identical_turn_captures_to_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ModelConfig::new("gemma-4-E4B-it-qat-UD-Q4_K_XL");
        let msgs = vec![Message::user().with_text("what is the weather?")];
        let tools = vec![tool("giap-weather__get_current_weather")];

        let first =
            write_payload_capture(dir.path(), &cfg, "SYSTEM", &msgs, &tools).expect("captured");
        let second =
            write_payload_capture(dir.path(), &cfg, "SYSTEM", &msgs, &tools).expect("captured");

        // The sequence number differs, the content hash does not.
        assert_ne!(first, second, "each call gets its own sequence number");
        let hash_of = |p: &std::path::Path| {
            p.file_stem()
                .unwrap()
                .to_str()
                .unwrap()
                .rsplit_once('-')
                .unwrap()
                .1
                .to_string()
        };
        assert_eq!(
            hash_of(&first),
            hash_of(&second),
            "an identical payload must hash identically, or prefix-stability findings are noise"
        );

        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&first).unwrap()).unwrap();
        assert_eq!(body["stream"], serde_json::json!(true));
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert!(
            body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "system"),
            "the captured body must carry the shim-enforced system prompt: replaying without \
             it measures a different prompt than GIAP sends"
        );
    }

    /// A payload is captured only when asked for. The hook sits on the hot path of every
    /// provider call, so an unset variable must not touch the filesystem.
    #[test]
    fn no_capture_directory_means_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        capture_payload(
            &ModelConfig::new("m"),
            "SYSTEM",
            &[Message::user().with_text("hi")],
            &[],
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
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

    // ── minification cache ─────────────────────────────────────────────────

    /// A tool carrying the keys `minify_schema_object` strips, so the cached
    /// answer is a `Some` rather than the less interesting `None`.
    fn noisy_tool(name: &str) -> Tool {
        Tool::new(
            name.to_string(),
            "desc".to_string(),
            rmcp::object!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "title": "Params",
                "type": "object",
                "properties": { "q": { "type": "string", "title": "Query" } }
            }),
        )
    }

    fn shim_with(controls: ShimControls) -> GiapProviderShim {
        // `Provider` requires only `get_name` and `stream`; the cache tests
        // exercise neither, so both are left unreachable rather than mocked.
        struct Unused;
        #[async_trait::async_trait]
        impl Provider for Unused {
            fn get_name(&self) -> &str {
                "unused"
            }
            async fn stream(
                &self,
                _: &goose_providers::model::ModelConfig,
                _: &str,
                _: &[Message],
                _: &[Tool],
            ) -> Result<goose::providers::base::MessageStream, ProviderError> {
                unreachable!("the cache tests never reach the inner provider")
            }
        }
        GiapProviderShim::new(Arc::new(Unused), Arc::new(controls), None)
    }

    #[test]
    fn an_unchanged_tool_set_is_minified_once_and_then_replayed() {
        let shim = shim_with(ShimControls::default());
        let tools = vec![noisy_tool("giap-weather__get_current_weather")];

        let first = shim
            .minify_tools_cached(&tools)
            .expect("minification fired");
        let second = shim.minify_tools_cached(&tools).expect("cache hit");
        assert_eq!(first, second);
        // The stripped keys really are gone, so the cached value is the
        // minified one and not a pass-through of the input.
        assert!(first[0].input_schema.get("$schema").is_none());
        assert!(first[0].input_schema.get("title").is_none());
    }

    /// The cache must key on the SCHEMAS, not the names. A narrowed allow-set
    /// arrives here as a shorter slice, and a re-registered extension arrives as
    /// the same names behind fresh allocations; both must miss rather than
    /// replay a stale tool set to the model.
    #[test]
    fn a_different_tool_set_is_never_served_from_the_cache() {
        let shim = shim_with(ShimControls::default());
        let both = vec![
            noisy_tool("giap-weather__get_current_weather"),
            noisy_tool("giap-memory__recall_memories"),
        ];
        let narrowed = vec![noisy_tool("giap-weather__get_current_weather")];

        let wide = shim.minify_tools_cached(&both).expect("minification fired");
        assert_eq!(wide.len(), 2);

        let narrow = shim
            .minify_tools_cached(&narrowed)
            .expect("minification fired");
        assert_eq!(
            narrow.len(),
            1,
            "the narrowed set was served from the cache"
        );

        // And back again, to prove the cache is replaced rather than appended.
        let wide_again = shim.minify_tools_cached(&both).expect("minification fired");
        assert_eq!(wide_again.len(), 2);
    }

    /// Same names, same schema CONTENT, different allocations. Pointer identity
    /// makes this a miss, which is the conservative direction: it costs one
    /// extra minification and can never serve the wrong schemas.
    #[test]
    fn structurally_equal_tools_behind_new_allocations_are_recomputed_not_replayed() {
        let shim = shim_with(ShimControls::default());
        let first_set = vec![noisy_tool("giap-weather__get_current_weather")];
        let rebuilt = vec![noisy_tool("giap-weather__get_current_weather")];

        let a = shim.minify_tools_cached(&first_set).expect("fired");
        let b = shim.minify_tools_cached(&rebuilt).expect("fired");
        assert_eq!(a, b, "a recompute must agree with the cached answer");
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

    // ── PAI-6 P3 ────────────────────────────────────────────────────────────

    /// Every subagent run mints an entry in a map that evicts oldest-first regardless of liveness.
    /// Releasing a child's entry when its run ends is what stops a stream of delegations taking
    /// a long-running PARENT's allow-set, after which its turns are pass-through and a Guest's
    /// `subtract_guest_denied_tools` result goes with them.
    #[test]
    fn releasing_children_keeps_a_live_parents_allow_set() {
        let controls = ShimControls::default();
        controls.session("parent").set_allowed_tools(
            ["giap-weather__get_forecast".to_string()]
                .into_iter()
                .collect(),
        );

        for i in 0..(MAX_TRACKED_SESSIONS * 2) {
            let child_id = format!("child-{i}");
            controls
                .session(&child_id)
                .set_allowed_tools(HashSet::new());
            controls.forget_session(&child_id);
        }

        let parent = controls
            .existing_session("parent")
            .expect("the parent's entry was evicted by children that had already finished");
        assert_eq!(
            parent.allowed_tools_snapshot(),
            Some(
                ["giap-weather__get_forecast".to_string()]
                    .into_iter()
                    .collect()
            )
        );
        assert_eq!(controls.tracked_sessions(), 1);
    }

    /// Vacuity control for the test above: without the release the parent IS
    /// evicted, so `forget_session` is doing the work rather than the cap
    /// happening never to be reached.
    #[test]
    fn without_releasing_them_children_do_evict_a_live_parent() {
        let controls = ShimControls::default();
        controls.session("parent").set_allowed_tools(
            ["giap-weather__get_forecast".to_string()]
                .into_iter()
                .collect(),
        );
        for i in 0..(MAX_TRACKED_SESSIONS * 2) {
            controls.session(&format!("child-{i}"));
        }
        assert!(
            controls.existing_session("parent").is_none(),
            "the map no longer evicts, so the test next door proves nothing"
        );
    }

    #[test]
    fn forgetting_one_session_leaves_the_others_alone() {
        let controls = ShimControls::default();
        controls.session("a");
        controls.session("b");
        controls.forget_session("a");
        assert!(controls.existing_session("a").is_none());
        assert!(controls.existing_session("b").is_some());
        // Idempotent: releasing a child twice, or one that never existed, is
        // not an error -- `release` runs on every path out of a run.
        controls.forget_session("a");
        controls.forget_session("never-existed");
        assert_eq!(controls.tracked_sessions(), 1);
    }

    /// Captures what actually reached the inner provider. These tests drive the REAL
    /// `Provider::stream`, not `enforce_system`, because the subagent override is resolved inside
    /// `stream` and the direct `enforce_system` tests cannot see it; the defect was invisible to
    /// every existing test for exactly that reason.
    struct Capturing {
        seen: Arc<Mutex<Option<(String, Vec<String>)>>>,
    }

    #[async_trait]
    impl Provider for Capturing {
        fn get_name(&self) -> &str {
            "capturing"
        }
        async fn stream(
            &self,
            _: &ModelConfig,
            system: &str,
            _: &[Message],
            tools: &[Tool],
        ) -> Result<MessageStream, ProviderError> {
            *self.seen.lock().unwrap() = Some((
                system.to_string(),
                tools.iter().map(|t| t.name.to_string()).collect(),
            ));
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    fn child_system_prompt() -> String {
        format!(
            "{PREFIX}\n\n# Delegated task\n\nYou have at most 4 turns.\n\
             - You cannot delegate. There is no one below you.\n\
             - Your only tools are: giap-weather__get_forecast.\n"
        )
    }

    async fn stream_as(
        session_id: &str,
        controls: Arc<ShimControls>,
        system: &str,
    ) -> (String, Vec<String>) {
        let seen = Arc::new(Mutex::new(None));
        let shim =
            GiapProviderShim::new(Arc::new(Capturing { seen: seen.clone() }), controls, None);
        let tools = vec![
            Tool::new(
                "giap-weather__get_forecast".to_string(),
                "d".to_string(),
                rmcp::object!({"type": "object"}),
            ),
            Tool::new(
                "giap-memory__forget_memory".to_string(),
                "d".to_string(),
                rmcp::object!({"type": "object"}),
            ),
        ];
        goose::session_context::with_session_id(Some(session_id.to_string()), async {
            let _ = shim
                .stream(&ModelConfig::new("m"), system, &[], &tools)
                .await;
        })
        .await;
        let captured = seen.lock().unwrap().clone();
        captured.expect("the inner provider was never reached")
    }

    // ── The egress gate (PAI-2) ─────────────────────────────────────────
    // These tests are the gate's only guard: the inner provider is submodule code sending its
    // own reqwest HTTP, so the workspace's egress_guard test cannot see it and would not fail
    // if the gate were deleted. These do.

    /// `network_mode` is process-global, so the tests that set it take this
    /// lock and restore Open before releasing — without it, parallel test
    /// threads race the mode and the failures point at the wrong test.
    static NETWORK_MODE_LOCK: Mutex<()> = Mutex::new(());

    /// A provider that remembers whether the call got through the gate.
    struct Reached(Arc<Mutex<bool>>);

    #[async_trait]
    impl Provider for Reached {
        fn get_name(&self) -> &str {
            "reached"
        }
        async fn stream(
            &self,
            _: &ModelConfig,
            _: &str,
            _: &[Message],
            _: &[Tool],
        ) -> Result<MessageStream, ProviderError> {
            *self.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    fn gated_shim(endpoint: Option<&str>) -> (GiapProviderShim, Arc<Mutex<bool>>) {
        let reached = Arc::new(Mutex::new(false));
        let shim = GiapProviderShim::new(
            Arc::new(Reached(reached.clone())),
            Arc::new(ShimControls::default()),
            endpoint.map(str::to_string),
        );
        (shim, reached)
    }

    async fn stream_once(shim: &GiapProviderShim) -> Result<(), ProviderError> {
        shim.stream(&ModelConfig::new("m"), "s", &[], &[])
            .await
            .map(|_| ())
    }

    /// The hole this gate closes, measured before it existed: a pond with
    /// `chat_provider = ollama` and `GIAP_OLLAMA_URL` pointed off-box shipped
    /// every conversation — extracted memories included — with no gate, no
    /// record, and offline mode not stopping it.
    #[tokio::test]
    async fn offline_mode_refuses_a_remote_model_host_before_a_packet_leaves() {
        use pond_core::shared::services::egress::{set_network_mode, NetworkMode};
        let _guard = NETWORK_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_network_mode(NetworkMode::Offline);

        let (shim, reached) = gated_shim(Some("http://gpu-box.tailnet.example:11434"));
        let err = stream_once(&shim)
            .await
            .expect_err("the gate did not refuse");

        set_network_mode(NetworkMode::Open);
        assert!(
            matches!(err, ProviderError::RequestFailed(_)),
            "a policy denial must not be a retryable error class: {err:?}"
        );
        assert!(
            !*reached.lock().unwrap_or_else(|e| e.into_inner()),
            "the inner provider was reached — the refusal happened after the send"
        );
    }

    /// Offline means loopback-only, not silence: the normal install's own
    /// model server keeps answering.
    #[tokio::test]
    async fn offline_mode_still_reaches_a_loopback_model_server() {
        use pond_core::shared::services::egress::{set_network_mode, NetworkMode};
        let _guard = NETWORK_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_network_mode(NetworkMode::Offline);

        let (shim, reached) = gated_shim(Some("http://127.0.0.1:8080"));
        let result = stream_once(&shim).await;

        set_network_mode(NetworkMode::Open);
        result.expect("loopback must pass in offline mode");
        assert!(*reached.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// Allowlist refuses hosts that classify as Sensitive — a remote model box
    /// is exactly that class.
    #[tokio::test]
    async fn allowlist_mode_refuses_a_remote_model_host() {
        use pond_core::shared::services::egress::{set_network_mode, NetworkMode};
        let _guard = NETWORK_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_network_mode(NetworkMode::Allowlist);

        let (shim, reached) = gated_shim(Some("http://gpu-box.tailnet.example:11434"));
        let err = stream_once(&shim)
            .await
            .expect_err("allowlist did not refuse");

        set_network_mode(NetworkMode::Open);
        assert!(matches!(err, ProviderError::RequestFailed(_)), "{err:?}");
        assert!(!*reached.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// In-process inference has no wire. `None` must mean "not gated", or
    /// offline mode would refuse the one provider that never leaves the box.
    #[tokio::test]
    async fn a_provider_with_no_endpoint_is_not_gated_even_offline() {
        use pond_core::shared::services::egress::{set_network_mode, NetworkMode};
        let _guard = NETWORK_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_network_mode(NetworkMode::Offline);

        let (shim, reached) = gated_shim(None);
        let result = stream_once(&shim).await;

        set_network_mode(NetworkMode::Open);
        result.expect("in-process inference must not be gated");
        assert!(*reached.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// The metadata fetches reach the same host the chat does.
    #[tokio::test]
    async fn model_listing_is_gated_like_the_chat_is() {
        use pond_core::shared::services::egress::{set_network_mode, NetworkMode};
        let _guard = NETWORK_MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_network_mode(NetworkMode::Offline);

        let (shim, _) = gated_shim(Some("http://gpu-box.tailnet.example:11434"));
        let err = shim.fetch_supported_models().await;

        set_network_mode(NetworkMode::Open);
        assert!(
            matches!(err, Err(ProviderError::RequestFailed(_))),
            "model listing bypassed the gate: {err:?}"
        );
    }

    /// A child's system prompt is the parent's static prefix plus GIAP's delegation envelope, so
    /// `incoming.starts_with(prefix)` matches; without an override the rebuild throws the
    /// envelope (turn budget, no-delegation rule, exact tools held) away and splices in the
    /// GLOBAL extension appendix instead.
    #[tokio::test]
    async fn a_subagent_sessions_system_prompt_reaches_the_provider_intact() {
        let controls = Arc::new(ShimControls::default());
        controls.set_system_prefix(PREFIX.to_string());
        controls.set_extension_appendix(Some("# MCP Extensions\n- music".to_string()));

        let child_system = child_system_prompt();
        let child = controls.session("child-1");
        child.set_system_override(Some(child_system.clone()));
        child.set_allowed_tools(
            ["giap-weather__get_forecast".to_string()]
                .into_iter()
                .collect(),
        );

        let (system, tools) = stream_as("child-1", controls, &child_system).await;
        assert_eq!(
            system, child_system,
            "the subagent's delegation envelope did not survive the shim"
        );
        assert!(
            !system.contains("# MCP Extensions"),
            "a child was told about extensions it does not have"
        );
        assert_eq!(
            tools,
            vec!["giap-weather__get_forecast".to_string()],
            "the child's allow-set was not enforced at the provider, so a denied tool was \
             LISTED to it even though dispatch would have refused the call"
        );
    }

    /// Vacuity control, and proof the defect was real: the same child prompt and session with no
    /// override has its envelope destroyed and the global appendix arrives instead. If this ever
    /// stops happening, the override has become decoration and the test above asserts nothing.
    #[tokio::test]
    async fn without_the_override_the_shim_destroys_a_childs_prompt() {
        let controls = Arc::new(ShimControls::default());
        controls.set_system_prefix(PREFIX.to_string());
        controls.set_extension_appendix(Some("# MCP Extensions\n- music".to_string()));
        // An entry exists -- this is not the pass-through path -- it simply
        // does not claim to own its system prompt.
        controls.session("child-1");

        let child_system = child_system_prompt();
        let (system, _) = stream_as("child-1", controls, &child_system).await;
        assert!(
            !system.contains("You cannot delegate"),
            "the rebuild kept the envelope, so the override is not what preserves it"
        );
        assert!(system.contains("# MCP Extensions"));
    }
}
