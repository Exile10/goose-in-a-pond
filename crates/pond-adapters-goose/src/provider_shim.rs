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

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

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

/// Mutable per-turn controls shared between [`GooseAdapter`] and the shim.
///
/// The adapter writes these while assembling a chat turn; the shim reads them
/// inside `Provider::stream`. All `None` means "no enforcement" (pass-through).
#[derive(Default)]
pub struct ShimControls {
    /// GIAP's authoritative static system prefix for the current chat model —
    /// the exact string passed to `override_system_prompt`.
    system_prefix: Mutex<Option<String>>,
    /// Per-turn GIAP-owned appendix (prompt extras + skills), rebuilt fresh
    /// each turn. Kept separate from the prefix so the veto can truncate
    /// Goose's extras without losing GIAP's own.
    turn_appendix: Mutex<Option<String>>,
    /// External MCP extension listing — recomputed only when the tool cache
    /// refreshes, so it persists across turns (same lifetime as the goose
    /// extra it mirrors).
    extension_appendix: Mutex<Option<String>>,
    /// Exact (prefixed) tool names allowed this turn. `None` disables tool
    /// filtering entirely.
    allowed_tools: Mutex<Option<HashSet<String>>>,
}

impl ShimControls {
    pub fn set_system_prefix(&self, prefix: String) {
        *self.system_prefix.lock().unwrap_or_else(|e| e.into_inner()) = Some(prefix);
    }

    pub fn set_turn_appendix(&self, appendix: Option<String>) {
        *self.turn_appendix.lock().unwrap_or_else(|e| e.into_inner()) = appendix;
    }

    pub fn set_extension_appendix(&self, appendix: Option<String>) {
        *self
            .extension_appendix
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = appendix;
    }

    pub fn set_allowed_tools(&self, tools: HashSet<String>) {
        *self.allowed_tools.lock().unwrap_or_else(|e| e.into_inner()) = Some(tools);
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
        let (prefix, turn_apx, ext_apx, allowed) = {
            (
                self.controls
                    .system_prefix
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
                self.controls
                    .turn_appendix
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
                self.controls
                    .extension_appendix
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
                self.controls
                    .allowed_tools
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            )
        };

        let enforced_system = enforce_system(system, &prefix, &[&turn_apx, &ext_apx]);
        let stripped_messages = strip_turn_context(messages);
        let vetoed_tools = enforce_tools(tools, &allowed);
        // Minify AFTER the veto so we never pay for tools about to be dropped.
        let minified_tools = minify_tools(vetoed_tools.as_deref().unwrap_or(tools));
        let final_tools: &[Tool] = minified_tools
            .as_deref()
            .or(vetoed_tools.as_deref())
            .unwrap_or(tools);

        if enforced_system.is_some() || stripped_messages.is_some() || vetoed_tools.is_some() {
            tracing::debug!(
                system_rebuilt = enforced_system.is_some(),
                turn_context_stripped = stripped_messages.is_some(),
                tools_vetoed = vetoed_tools.is_some(),
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
                tools_count = final_tools.len(),
                tools_json_chars = tools_chars,
                "provider payload size"
            );
        }

        self.inner
            .stream(
                model_config,
                enforced_system.as_deref().unwrap_or(system),
                stripped_messages.as_deref().unwrap_or(messages),
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
}
