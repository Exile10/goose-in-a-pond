//! Reading the calling session out of an MCP request.
//!
//! Goose stamps every `CallToolRequest` with the session it is serving:
//! `mcp_client.rs::inject_session_context_into_extensions` inserts
//! `agent-session-id` into the request's `Meta`, rmcp serialises extension-level
//! `Meta` out as the wire `_meta`, and rmcp's serve loop swaps it into the
//! `RequestContext.meta` handed to the tool handler.
//!
//! This is the only trustworthy per-call session channel a GIAP builtin has.
//! The alternatives, and why not:
//!
//! - **The tool's own `session_id` parameter** -- filled in by the MODEL. It is
//!   how `save_draft` and `list_drafts` used to scope, and it defaulted to the
//!   literal "default", so every draft on the pond shared one bucket.
//! - **`crate::current_session_id()`** -- a process-global `RwLock<String>`
//!   with `Semaphore::new(4)` chat streams racing it. Correct authorisation on
//!   a misattributed session is not correct.
//! - **A per-session server instance** -- impossible: `SpawnServerFn` is
//!   `fn(DuplexStream, DuplexStream)`, and Goose's extension manager
//!   early-returns for an unchanged config, so one instance serves the process.
//!
//! The key is Goose's `SESSION_ID_HEADER`, duplicated here as a const rather
//! than imported: `pond-mcp-server` must not depend on the goose submodule.

use rmcp::model::Meta;

/// The `_meta` key the engine stamps the session id under.
pub const SESSION_ID_META_KEY: &str = "agent-session-id";

/// The engine session id behind this tool call, if the engine supplied one.
///
/// Matched case-insensitively because the value travels as a header-shaped key
/// and goose's own removal pass uses `eq_ignore_ascii_case`. Blank is `None`:
/// an empty string is not a session, and letting it through would make every
/// unbound caller look like one shared session.
pub fn session_from_meta(meta: &Meta) -> Option<String> {
    meta.0
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(SESSION_ID_META_KEY))
        .and_then(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn meta_with(key: &str, value: &str) -> Meta {
        let mut m = Meta::new();
        m.0.insert(key.to_string(), Value::String(value.to_string()));
        m
    }

    #[test]
    fn reads_the_engine_key() {
        assert_eq!(
            session_from_meta(&meta_with(SESSION_ID_META_KEY, "20260805_7")),
            Some("20260805_7".to_string())
        );
    }

    #[test]
    fn matches_the_key_case_insensitively() {
        assert_eq!(
            session_from_meta(&meta_with("Agent-Session-Id", "20260805_7")),
            Some("20260805_7".to_string())
        );
    }

    #[test]
    fn blank_and_absent_are_both_none() {
        assert_eq!(
            session_from_meta(&meta_with(SESSION_ID_META_KEY, "   ")),
            None
        );
        assert_eq!(session_from_meta(&Meta::new()), None);
        assert_eq!(session_from_meta(&meta_with("progressToken", "x")), None);
    }
}
