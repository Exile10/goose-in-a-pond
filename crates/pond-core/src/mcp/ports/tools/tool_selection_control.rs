//! Port for the tool-relevance escape hatch (Phase D2).
//!
//! Phase D narrows which extension tool schemas reach the model, per session, to
//! fit an 8K-class on-device prompt budget. That is only safe if the model can
//! reach a capability that was not preloaded — otherwise a mis-scored session is
//! a dead end.
//!
//! This port is that escape hatch. It is driven by the `giap-toolkit` MCP
//! extension (always in the core set) and implemented by whichever agent adapter
//! owns the per-session selection.

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolSelectionError {
    #[error("unknown tool group '{0}'")]
    UnknownGroup(String),
    #[error("tool group '{0}' is not available on this device")]
    GroupNotRegistered(String),
    #[error("tool selection is not active for this session")]
    NotActive,
    /// The group was recorded but its tools cannot reach the model this turn,
    /// because the live tool cache was cold when the widen happened.
    ///
    /// Its own variant rather than a silent `Ok`, because the caller's success
    /// message is *"its tools are available now — go ahead and call the one you
    /// need"*. A model told that, which then calls and is suppressed by the
    /// tool-call guard, has spent a turn of a small budget on a sentence that was
    /// not true. Saying "next turn" costs a sentence instead.
    #[error("tool group '{0}' is loaded but its tools arrive on the next turn")]
    NotReady(String),
    #[error("tool selection failed: {0}")]
    Internal(String),
}

/// One row of the catalog as the model sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolGroupStatus {
    pub extension: String,
    pub description: String,
    /// Tools are in the prompt right now.
    pub loaded: bool,
    /// Always loaded, cannot be turned off.
    pub core: bool,
    /// How many tools the group contributes, when known.
    pub tool_count: usize,
}

/// Driven port: inspect and widen a session's loaded tool groups.
///
/// **`engine_session_id` is the AGENT ENGINE's session id, not GIAP's**, and the
/// distinction is a correctness one rather than a naming one. An MCP tool's only
/// trustworthy fact about its caller is goose's `agent-session-id` in the request
/// `_meta` — `crates/pond-mcp-server/src/session_meta.rs` names and rejects the
/// alternatives, including the process-global `current_session_id()` this port's
/// caller used to read: a `RwLock<String>` with four concurrent chat streams
/// racing it, so one member's `enable_tool_group` could widen another member's
/// allow-set. "Correct authorisation on a misattributed session is not correct."
///
/// The implementor owns the translation to whatever it keys sessions by.
#[async_trait]
pub trait ToolSelectionControl: Send + Sync {
    /// Every registered group with its current loaded/dormant status for this
    /// session.
    async fn group_status(&self, engine_session_id: &str) -> Vec<ToolGroupStatus>;

    /// Load `group` for this session. Idempotent. Returns the session's full
    /// group list afterwards.
    ///
    /// Takes effect for the next provider call — including the next call of the
    /// turn that invoked it, so the model can enable and then immediately use a
    /// capability. Costs one prompt-prefix rebuild.
    async fn enable_group(
        &self,
        engine_session_id: &str,
        group: &str,
    ) -> Result<Vec<String>, ToolSelectionError>;
}
