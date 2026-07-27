//! Draft MCP Server — stage destructive actions for user confirmation.
//!
//! Provides 4 tools: `save_draft`, `list_drafts`, `approve_draft`, `reject_draft`.
//! Depends only on [`DraftRepository`] — no god-struct.
//!
//! The draft server is always enabled (not toggleable) because it is a safety feature.
//! Tool descriptions instruct the LLM to use `save_draft` instead of executing
//! destructive actions directly.

use pond_core::user_data::domain::draft::{Draft, DraftStatus};
use pond_core::user_data::ports::draft::DraftRepository;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SaveDraftParams {
    /// Tag, e.g. "shell_command", "file_write".
    pub kind: Option<String>,
    /// One-line summary shown to the user.
    pub summary: Option<String>,
    /// JSON string needed to execute later.
    pub payload: Option<String>,
    /// Auto-filled if omitted.
    pub session_id: Option<String>,
    /// Catch-all for unexpected fields.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListDraftsParams {
    /// Omit for all pending drafts.
    pub session_id: Option<String>,
    /// Catch-all for unexpected fields.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ApproveDraftParams {
    pub draft_id: Option<String>,
    /// Alias for draft_id.
    pub id: Option<String>,
    /// Catch-all for unexpected fields.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RejectDraftParams {
    pub draft_id: Option<String>,
    /// Alias for draft_id.
    pub id: Option<String>,
    /// Catch-all for unexpected fields.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DraftMcpServer {
    draft_repo: Arc<dyn DraftRepository + Send + Sync>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl DraftMcpServer {
    pub fn new(draft_repo: Arc<dyn DraftRepository + Send + Sync>) -> Self {
        Self {
            draft_repo,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Stage any side-effect action (shell, file write, schedule) as a draft for \
        user confirmation. Never execute destructive actions directly."
    )]
    async fn save_draft(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SaveDraftParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let kind = params
            .0
            .kind
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| extract_string_from_extras(&params.0.extra, &["kind", "type", "action"]))
            .unwrap_or("unknown")
            .to_string();

        let summary = params
            .0
            .summary
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                extract_string_from_extras(&params.0.extra, &["summary", "description", "desc"])
            })
            .unwrap_or("(no description)")
            .to_string();

        let payload = params
            .0
            .payload
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| extract_string_from_extras(&params.0.extra, &["payload", "data", "params"]))
            .unwrap_or("{}")
            .to_string();

        let session_id = params
            .0
            .session_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("default")
            .to_string();

        let draft_id = format!(
            "draft_{}_{}",
            chrono::Utc::now().timestamp_millis(),
            &uuid::Uuid::new_v4().to_string()[..8]
        );

        let draft = Draft {
            id: draft_id.clone(),
            session_id,
            kind: kind.clone(),
            summary: summary.clone(),
            payload,
            status: DraftStatus::Pending,
            created_at: chrono::Utc::now(),
        };

        match self.draft_repo.save(draft).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Draft saved as {draft_id} ({kind}): {summary}\n\
                 Tell the user what you plan to do and ask them to confirm with \"approve\" or \"reject\"."
            ))])),
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Failed to save draft: {e}. Tell the user the action could not be staged."
            ))])),
        }
    }

    #[tool(description = "List pending drafts awaiting user confirmation.")]
    async fn list_drafts(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ListDraftsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let session_id = params
            .0
            .session_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("default");

        match self.draft_repo.list_pending(session_id).await {
            Ok(drafts) => {
                if drafts.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "No pending drafts.",
                    )]));
                }
                let text = drafts
                    .iter()
                    .map(|d| format!("[{}] ({}) {}", d.id, d.kind, d.summary))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Could not list drafts: {e}"
            ))])),
        }
    }

    #[tool(description = "Approve a pending draft for execution after the user confirms.")]
    async fn approve_draft(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ApproveDraftParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let draft_id = extract_draft_id(&params.0.draft_id, &params.0.id, &params.0.extra);

        let Some(draft_id) = draft_id else {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a draft_id to approve. Use list_drafts to see pending drafts.",
            )]));
        };

        // Verify the draft exists and is pending
        match self.draft_repo.get(&draft_id).await {
            Ok(Some(draft)) if draft.status == DraftStatus::Pending => {}
            Ok(Some(_)) => {
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Draft {draft_id} is not pending — it may have already been decided."
                ))]));
            }
            Ok(None) => {
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Draft {draft_id} not found."
                ))]));
            }
            Err(e) => {
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Could not look up draft: {e}"
                ))]));
            }
        }

        match self
            .draft_repo
            .update_status(&draft_id, DraftStatus::Approved)
            .await
        {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Draft {draft_id} approved."
            ))])),
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Failed to approve draft: {e}"
            ))])),
        }
    }

    #[tool(description = "Reject a pending draft after the user declines.")]
    async fn reject_draft(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<RejectDraftParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let draft_id = extract_draft_id(&params.0.draft_id, &params.0.id, &params.0.extra);

        let Some(draft_id) = draft_id else {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a draft_id to reject. Use list_drafts to see pending drafts.",
            )]));
        };

        match self
            .draft_repo
            .update_status(&draft_id, DraftStatus::Rejected)
            .await
        {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Draft {draft_id} rejected."
            ))])),
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Failed to reject draft: {e}"
            ))])),
        }
    }
}

#[tool_handler]
impl ServerHandler for DraftMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new("giap-draft", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "GIAP Draft MCP server — stage destructive actions for user confirmation.\n\n\
                 For any action with side effects (shell commands, file writes, schedule creation, \
                 device control), use save_draft instead of executing directly. The user will \
                 review the draft and approve or reject it.\n\n\
                 Tools: save_draft (stage an action), list_drafts (show pending), \
                 approve_draft (mark approved), reject_draft (mark rejected).",
            )
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Extract a string value from the extras map by trying multiple key names.
fn extract_string_from_extras<'a>(
    extras: &'a std::collections::HashMap<String, serde_json::Value>,
    keys: &[&str],
) -> Option<&'a str> {
    for key in keys {
        if let Some(val) = extras.get(*key) {
            if let Some(s) = val.as_str() {
                if !s.trim().is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// Extract draft ID from the various places a model might put it.
fn extract_draft_id(
    draft_id: &Option<String>,
    id: &Option<String>,
    extras: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<String> {
    // 1. Canonical field
    if let Some(ref d) = draft_id {
        if !d.trim().is_empty() {
            return Some(d.trim().to_string());
        }
    }
    // 2. Alternate field
    if let Some(ref d) = id {
        if !d.trim().is_empty() {
            return Some(d.trim().to_string());
        }
    }
    // 3. Extras scan
    for key in &["draft_id", "id", "draftId"] {
        if let Some(val) = extras.get(*key) {
            if let Some(s) = val.as_str() {
                if !s.trim().is_empty() {
                    return Some(s.trim().to_string());
                }
            }
        }
    }
    None
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct DraftDeps {
    draft_repo: Arc<dyn DraftRepository + Send + Sync>,
}

static DRAFT_DEPS: OnceLock<DraftDeps> = OnceLock::new();

/// Initialize draft server dependencies. Call once at startup.
pub fn init_draft_deps(draft_repo: Arc<dyn DraftRepository + Send + Sync>) {
    let _ = DRAFT_DEPS.set(DraftDeps { draft_repo });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_draft_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = DRAFT_DEPS.get().expect("init_draft_deps() not called");
    let server = DraftMcpServer::new(deps.draft_repo.clone());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-draft MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::user_data::domain::draft::{Draft, DraftStatus};
    use pond_core::user_data::ports::draft::DraftRepository;
    use std::sync::Mutex;

    struct StubDraftRepo {
        drafts: Mutex<Vec<Draft>>,
    }

    impl StubDraftRepo {
        fn new() -> Self {
            Self {
                drafts: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl DraftRepository for StubDraftRepo {
        async fn save(&self, draft: Draft) -> anyhow::Result<()> {
            self.drafts.lock().unwrap().push(draft);
            Ok(())
        }

        async fn list_pending(&self, session_id: &str) -> anyhow::Result<Vec<Draft>> {
            let guard = self.drafts.lock().unwrap();
            Ok(guard
                .iter()
                .filter(|d| d.session_id == session_id && d.status == DraftStatus::Pending)
                .cloned()
                .collect())
        }

        async fn get(&self, id: &str) -> anyhow::Result<Option<Draft>> {
            let guard = self.drafts.lock().unwrap();
            Ok(guard.iter().find(|d| d.id == id).cloned())
        }

        async fn update_status(&self, id: &str, status: DraftStatus) -> anyhow::Result<()> {
            let mut guard = self.drafts.lock().unwrap();
            if let Some(d) = guard.iter_mut().find(|d| d.id == id) {
                d.status = status;
                Ok(())
            } else {
                anyhow::bail!("not found")
            }
        }
    }

    #[test]
    fn server_constructs() {
        let _server = DraftMcpServer::new(Arc::new(StubDraftRepo::new()));
    }

    #[test]
    fn extract_draft_id_from_canonical() {
        let id = extract_draft_id(
            &Some("draft_123".to_string()),
            &None,
            &std::collections::HashMap::new(),
        );
        assert_eq!(id, Some("draft_123".to_string()));
    }

    #[test]
    fn extract_draft_id_from_alt() {
        let id = extract_draft_id(
            &None,
            &Some("draft_456".to_string()),
            &std::collections::HashMap::new(),
        );
        assert_eq!(id, Some("draft_456".to_string()));
    }

    #[test]
    fn extract_draft_id_from_extras() {
        let mut extras = std::collections::HashMap::new();
        extras.insert(
            "draftId".to_string(),
            serde_json::Value::String("draft_789".to_string()),
        );
        let id = extract_draft_id(&None, &None, &extras);
        assert_eq!(id, Some("draft_789".to_string()));
    }

    #[test]
    fn extract_draft_id_returns_none_when_empty() {
        let id = extract_draft_id(&None, &None, &std::collections::HashMap::new());
        assert!(id.is_none());
    }
}
