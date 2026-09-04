//! Port for the tool-relevance escape hatch (Phase D2). Phase D narrows which extension tool
//! schemas reach the model per session, to fit an 8K-class on-device prompt budget; that is only
//! safe if the model can still reach a capability that was not preloaded. Driven by the
//! `giap-toolkit` extension (always core), implemented by the adapter owning per-session selection.

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
    /// The group was recorded but its tools cannot reach the model this turn, because the live
    /// tool cache was cold when the widen happened. Its own variant rather than a silent `Ok`: a
    /// model told the tools are available now calls one, is suppressed by the tool-call guard, and
    /// spends a turn of a small budget on a sentence that was not true.
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

/// Driven port: inspect and widen a session's loaded tool groups. `engine_session_id` is the AGENT
/// ENGINE's session id, not GIAP's: the only trustworthy fact about a caller is goose's
/// `agent-session-id` in the request `_meta` (see `pond-mcp-server/src/session_meta.rs`). The
/// process-global `current_session_id()` races concurrent streams and widens the wrong allow-set.
#[async_trait]
pub trait ToolSelectionControl: Send + Sync {
    /// Every registered group with its current loaded/dormant status for this
    /// session.
    async fn group_status(&self, engine_session_id: &str) -> Vec<ToolGroupStatus>;

    /// Load `group` for this session. Idempotent; returns the session's full group list. Takes
    /// effect for the next provider call, including the next call of the turn that invoked it, so
    /// the model can enable a capability and then use it. Costs one prompt-prefix rebuild.
    async fn enable_group(
        &self,
        engine_session_id: &str,
        group: &str,
    ) -> Result<Vec<String>, ToolSelectionError>;
}
