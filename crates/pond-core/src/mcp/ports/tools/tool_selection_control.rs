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
#[async_trait]
pub trait ToolSelectionControl: Send + Sync {
    /// Every registered group with its current loaded/dormant status for this
    /// session.
    async fn group_status(&self, session_id: &str) -> Vec<ToolGroupStatus>;

    /// Load `group` for this session. Idempotent. Returns the session's full
    /// group list afterwards.
    ///
    /// Takes effect for the next provider call — including the next call of the
    /// turn that invoked it, so the model can enable and then immediately use a
    /// capability. Costs one prompt-prefix rebuild.
    async fn enable_group(
        &self,
        session_id: &str,
        group: &str,
    ) -> Result<Vec<String>, ToolSelectionError>;
}
