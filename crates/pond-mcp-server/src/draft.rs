//! Draft MCP Server — stage destructive actions for user confirmation.
//!
//! Provides 4 tools: `save_draft`, `list_drafts`, `approve_draft`, `reject_draft`.
//! Depends only on [`DraftRepository`] — no god-struct.
//!
//! The draft server is always enabled (not toggleable) because it is a safety feature.
//! Tool descriptions instruct the LLM to use `save_draft` instead of executing
//! destructive actions directly.

use pond_core::security::ports::draft_authority::DraftAuthority;
use pond_core::security::ports::policy::{is_draft_decision_permitted, PolicyDecision, PolicyMode};
use pond_core::user_data::domain::draft::{Draft, DraftStatus};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::session::IdentificationSource;
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
    /// Resolves the caller and the mode. `None` only in tests: every decision is
    /// then unresolvable, which refuses under `enforce` and reports `would_deny`
    /// under `audit` -- meaning it is **let through**, because `would_deny` is
    /// audit's whole point. Any constructor that serves real callers must pass
    /// [`draft_authority`]; the direct dispatcher did not, and that was a
    /// bypass, not a quarantine.
    authority: Option<Arc<dyn DraftAuthority>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl DraftMcpServer {
    pub fn new(draft_repo: Arc<dyn DraftRepository + Send + Sync>) -> Self {
        Self {
            draft_repo,
            authority: None,
            tool_router: Self::tool_router(),
        }
    }

    /// Attach the authority that resolves the caller and the policy mode.
    pub fn with_authority(mut self, authority: Option<Arc<dyn DraftAuthority>>) -> Self {
        self.authority = authority;
        self
    }

    /// Who is calling, from the engine-stamped `_meta`, plus their scope and
    /// how it was established.
    ///
    /// Returns the session even when the scope is unresolvable, because the
    /// session alone still scopes an unowned draft.
    ///
    /// Resolved **once** per tool call and threaded, never re-resolved. Two
    /// calls straddling an identification write would disagree, and `save_draft`
    /// would then stamp a `profile_id` from one answer and an
    /// `identification_source` from the other -- a provenance that never
    /// happened, which is worse than no provenance at all (PAI-1 invariant 3).
    async fn caller(
        &self,
        meta: &rmcp::model::Meta,
    ) -> (String, Option<(ProfileScope, IdentificationSource)>) {
        let Some(session) = crate::session_from_meta(meta) else {
            tracing::warn!(
                target: "giap::trace",
                kind = "draft_caller_unknown",
                "a draft tool ran with no engine session in _meta"
            );
            return (String::new(), None);
        };
        let actor = match &self.authority {
            Some(auth) => auth.actor_for_engine_session(&session).await,
            None => None,
        };
        (session, actor)
    }

    /// Shared body of approve and reject, so the ownership check cannot be
    /// added to one and forgotten on the other.
    async fn decide(
        &self,
        meta: &rmcp::model::Meta,
        draft_id: &str,
        new_status: DraftStatus,
    ) -> String {
        let verb = match new_status {
            DraftStatus::Approved => "approve",
            DraftStatus::Rejected => "reject",
            _ => "decide",
        };

        let draft = match self.draft_repo.get(draft_id).await {
            Ok(Some(d)) => d,
            Ok(None) => return format!("Draft {draft_id} not found."),
            Err(e) => return format!("Could not look up draft: {e}"),
        };
        // Applied to reject as well as approve. reject_draft used to check
        // nothing at all, so it would flip an already-Approved draft to
        // Rejected long after the action had been authorised.
        if draft.status != DraftStatus::Pending {
            return format!("Draft {draft_id} is not pending — it may have already been decided.");
        }
        // PAI-7 invariant 7, on the one path that can authorise a staged action.
        //
        // Approval only. Rejecting an expired proposal must stay possible:
        // section 3.5 writes rejections back as memories, and that is how "we
        // never want to be told about this" becomes a learned constraint rather
        // than a setting nobody finds.
        //
        // This is the legible layer, not the load-bearing one. It is a branch
        // and a branch has a call site, so migration 0041 carries the same rule
        // as a BEFORE UPDATE trigger that aborts the transition -- which is
        // what still holds if this block is deleted, and what covers the
        // repositories that know nothing about proposals.
        if new_status == DraftStatus::Approved && !draft.is_live_at(chrono::Utc::now()) {
            return format!(
                "Draft {draft_id} has expired and can no longer be approved. Tell the user the \
                 suggestion is stale, and offer to look at it fresh if it still matters."
            );
        }

        let mode = match &self.authority {
            Some(auth) => auth.policy_mode().await,
            None => PolicyMode::Audit,
        };
        if mode == PolicyMode::Off {
            return self.apply(draft_id, new_status, verb).await;
        }

        let (session, actor) = self.caller(meta).await;
        let decision = match is_draft_decision_permitted(
            actor.as_ref().map(|(scope, _)| scope),
            &session,
            draft.profile_id.as_deref(),
            &draft.session_id,
        ) {
            Ok(()) => PolicyDecision::permit(mode),
            Err(reason) => PolicyDecision::refuse(mode, reason),
        };

        // Tallied at the decision site, not inside an `audit` implementation:
        // this is the count of what the policy decided, and it must not depend
        // on whether an audit sink happens to be installed. `POLICY_COUNTERS`
        // survives log pruning and `DELETE /api/v1/activity`, which the event
        // half does not.
        pond_core::security::ports::policy::POLICY_COUNTERS.record(&decision);
        if let Some(auth) = &self.authority {
            // The verdict rides the decision, not a `:{verdict}` suffix on the
            // action string. The report groups on the attribute.
            auth.audit(&session, &format!("draft_{verb}"), &decision)
                .await;
        }
        if decision.would_deny() {
            tracing::warn!(
                target: "giap::trace",
                kind = "policy_would_deny",
                scope = pond_core::security::ports::policy::scopes::DRAFT,
                reason = decision.denied_reason,
                draft_id,
                "security policy would have denied this in enforce mode"
            );
        }
        if !decision.allowed {
            return format!(
                "You are not permitted to {verb} draft {draft_id}: {}. Tell the user that whoever \
                 staged this action has to confirm it themselves.",
                decision.denied_reason.unwrap_or("refused")
            );
        }
        self.apply(draft_id, new_status, verb).await
    }

    async fn apply(&self, draft_id: &str, new_status: DraftStatus, verb: &str) -> String {
        match self.draft_repo.update_status(draft_id, new_status).await {
            Ok(()) => format!("Draft {draft_id} {verb}d."),
            Err(e) => format!("Failed to {verb} draft: {e}"),
        }
    }

    /// The owner to stamp on a draft being staged, and how it was established.
    ///
    /// `Owner(id)` is the only scope that names a member. `Household` and
    /// `Guest` both give `None`, for opposite reasons, and an unowned draft is
    /// the narrower of the two readings -- so neither is stamped. The source
    /// rides the same resolution as the id, so it can never describe a
    /// different one, and it is dropped along with it: a provenance without a
    /// subject records how we identified nobody.
    fn owner_stamp(
        actor: Option<&(ProfileScope, IdentificationSource)>,
    ) -> (Option<String>, Option<IdentificationSource>) {
        match actor {
            Some((scope, source)) => match scope.owner_id() {
                Some(id) => (Some(id.to_string()), Some(*source)),
                None => (None, None),
            },
            None => (None, None),
        }
    }

    /// The session to scope by, engine-first.
    ///
    /// The `session_id` tool PARAMETER is left in the schema for compatibility
    /// and is now advisory only: it is a value the model fills in and it
    /// defaulted to the literal "default", which is why every draft on every
    /// pond shared one bucket.
    fn scope_session(caller_session: &str, param: Option<&str>) -> String {
        if !caller_session.is_empty() {
            return caller_session.to_string();
        }
        param
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("default")
            .to_string()
    }

    #[tool(
        description = "Stage any side-effect action (shell, file write, schedule) as a draft for \
        user confirmation. Never execute destructive actions directly."
    )]
    async fn save_draft(
        &self,
        ctx: RequestContext<RoleServer>,
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

        // The session comes from the engine, never from the model.
        let (caller_session, caller_actor) = self.caller(&ctx.meta).await;
        let session_id = Self::scope_session(&caller_session, params.0.session_id.as_deref());

        let (profile_id, identification_source) = Self::owner_stamp(caller_actor.as_ref());

        let draft_id = format!(
            "draft_{}_{}",
            chrono::Utc::now().timestamp_millis(),
            &uuid::Uuid::new_v4().to_string()[..8]
        );

        let draft = Draft {
            id: draft_id.clone(),
            session_id,
            profile_id,
            identification_source,
            kind: kind.clone(),
            summary: summary.clone(),
            payload,
            status: DraftStatus::Pending,
            created_at: chrono::Utc::now(),
            // A draft the user is being asked to confirm in the same breath
            // does not expire. Proposals do, and they are written by
            // `SqliteProposalRepository`, not here.
            expires_at: None,
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
        ctx: RequestContext<RoleServer>,
        params: Parameters<ListDraftsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // Scope to the calling session, not to the one the model asked for.
        // Honouring the parameter is what let one session enumerate another's
        // draft ids, which is what made the approve hole exploitable.
        let (caller_session, _) = self.caller(&ctx.meta).await;
        let session_id = Self::scope_session(&caller_session, params.0.session_id.as_deref());

        match self.draft_repo.list_pending(&session_id).await {
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
        ctx: RequestContext<RoleServer>,
        params: Parameters<ApproveDraftParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let draft_id = extract_draft_id(&params.0.draft_id, &params.0.id, &params.0.extra);

        let Some(draft_id) = draft_id else {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a draft_id to approve. Use list_drafts to see pending drafts.",
            )]));
        };

        Ok(CallToolResult::success(vec![Content::text(
            self.decide(&ctx.meta, &draft_id, DraftStatus::Approved)
                .await,
        )]))
    }

    #[tool(description = "Reject a pending draft after the user declines.")]
    async fn reject_draft(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<RejectDraftParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let draft_id = extract_draft_id(&params.0.draft_id, &params.0.id, &params.0.extra);

        let Some(draft_id) = draft_id else {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a draft_id to reject. Use list_drafts to see pending drafts.",
            )]));
        };

        Ok(CallToolResult::success(vec![Content::text(
            self.decide(&ctx.meta, &draft_id, DraftStatus::Rejected)
                .await,
        )]))
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

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct DraftDeps {
    draft_repo: Arc<dyn DraftRepository + Send + Sync>,
}

static DRAFT_DEPS: OnceLock<DraftDeps> = OnceLock::new();
static DRAFT_AUTHORITY: OnceLock<Option<Arc<dyn DraftAuthority>>> = OnceLock::new();

/// Install the authority that resolves a draft decision's caller and mode.
///
/// Separate from [`init_draft_deps`] for the same reason `init_toolkit_deps` is
/// separate: it needs three repositories the extension registration does not
/// carry. Call it once at startup, before the first turn. Absent, every draft
/// decision is unresolvable -- refused under `enforce`, `would_deny` under
/// `audit`.
pub fn init_draft_authority(authority: Option<Arc<dyn DraftAuthority>>) {
    let _ = DRAFT_AUTHORITY.set(authority);
}

/// The installed authority, for the other places that build a `DraftMcpServer`.
///
/// [`spawn_draft_server`] is not the only constructor: `McpToolDispatcher` builds
/// its own copy for the direct-dispatch routes. It used to build it without an
/// authority, which made every decision unresolvable and therefore *permitted*
/// under `audit` -- see [`DraftMcpServer::authority`].
pub(crate) fn draft_authority() -> Option<Arc<dyn DraftAuthority>> {
    DRAFT_AUTHORITY.get().cloned().flatten()
}

/// Initialize draft server dependencies. Call once at startup.
pub fn init_draft_deps(draft_repo: Arc<dyn DraftRepository + Send + Sync>) {
    let _ = DRAFT_DEPS.set(DraftDeps { draft_repo });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_draft_server(reader: DuplexStream, writer: DuplexStream) {
    // Missing deps = this path never initialised this extension (the voice/CLI
    // binary vs `serve` install different families). A skipped extension is a
    // logged, contained failure; a panic here took down every builtin server's
    // startup at once (2026-08-27, giap-context in the voice child).
    let Some(deps) = DRAFT_DEPS.get() else {
        tracing::error!(
            "spawn_draft_server called before init_draft_deps — extension will not start"
        );
        return;
    };
    let server = DraftMcpServer::new(deps.draft_repo.clone())
        .with_authority(DRAFT_AUTHORITY.get().cloned().flatten());
    crate::serve_builtin("giap-draft", server, reader, writer);
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

    /// Records `(action, verdict, ok)` per audit call. The verdict is captured
    /// separately from `ok` on purpose: under `audit` mode every entry has
    /// `ok == true`, so a stub that stored only the bool could not tell an
    /// allow from a would-deny and every assertion made against it would be
    /// satisfied by deleting the rule.
    struct StubAuthority {
        mode: PolicyMode,
        scope: Option<ProfileScope>,
        audited: Mutex<Vec<(String, String, bool)>>,
    }

    #[async_trait]
    impl DraftAuthority for StubAuthority {
        async fn policy_mode(&self) -> PolicyMode {
            self.mode
        }
        async fn actor_for_engine_session(
            &self,
            _engine_session_id: &str,
        ) -> Option<(ProfileScope, IdentificationSource)> {
            self.scope
                .clone()
                .map(|s| (s, IdentificationSource::Explicit))
        }
        async fn audit(&self, _session: &str, action: &str, decision: &PolicyDecision) {
            self.audited.lock().unwrap().push((
                action.to_string(),
                decision.verdict().to_string(),
                decision.allowed,
            ));
        }
    }

    fn pending_draft(id: &str, session: &str, owner: Option<&str>) -> Draft {
        Draft {
            id: id.to_string(),
            session_id: session.to_string(),
            profile_id: owner.map(str::to_string),
            identification_source: None,
            kind: "shell_command".to_string(),
            summary: "remove the scratch directory".to_string(),
            payload: "{}".to_string(),
            status: DraftStatus::Pending,
            created_at: chrono::Utc::now(),
            expires_at: None,
        }
    }

    /// A staged action with an expiry -- the shape PAI-7's proposals have.
    fn expiring_draft(id: &str, session: &str, owner: Option<&str>, live: bool) -> Draft {
        let mut d = pending_draft(id, session, owner);
        d.kind = "proposal".to_string();
        d.expires_at = Some(if live {
            chrono::Utc::now() + chrono::Duration::hours(1)
        } else {
            chrono::Utc::now() - chrono::Duration::hours(1)
        });
        d
    }

    fn meta_for(session: &str) -> rmcp::model::Meta {
        let mut m = rmcp::model::Meta::new();
        m.0.insert(
            crate::SESSION_ID_META_KEY.to_string(),
            serde_json::Value::String(session.to_string()),
        );
        m
    }

    #[test]
    fn server_constructs() {
        let _server = DraftMcpServer::new(Arc::new(StubDraftRepo::new()));
    }

    /// The reported hole, and the audit/enforce split, in one fixture: the same
    /// call against the same rows changes only with the mode.
    #[tokio::test]
    async fn approve_refuses_a_foreign_draft_under_enforce_and_records_would_deny_under_audit() {
        for (mode, expect_status) in [
            (PolicyMode::Enforce, DraftStatus::Pending),
            (PolicyMode::Audit, DraftStatus::Approved),
        ] {
            let repo = Arc::new(StubDraftRepo::new());
            repo.save(pending_draft("d1", "sess-a", Some("jerry")))
                .await
                .unwrap();
            let auth = Arc::new(StubAuthority {
                mode,
                scope: Some(ProfileScope::Owner("liz".into())),
                audited: Mutex::new(Vec::new()),
            });
            let server = DraftMcpServer::new(repo.clone())
                .with_authority(Some(auth.clone() as Arc<dyn DraftAuthority>));

            let text = server
                .decide(&meta_for("sess-a"), "d1", DraftStatus::Approved)
                .await;

            let after = repo.get("d1").await.unwrap().unwrap();
            assert_eq!(after.status, expect_status, "mode {mode:?}");
            let audited = auth.audited.lock().unwrap().clone();
            assert_eq!(audited.len(), 1);
            // The action string is a plain verb in every mode now: the verdict
            // moved onto the decision so the policy report can group on an
            // attribute instead of parsing a substring out of a free-form field.
            assert_eq!(audited[0].0, "draft_approve", "mode {mode:?}");
            match mode {
                PolicyMode::Enforce => {
                    assert!(text.contains("not permitted"), "got: {text}");
                    assert_eq!(audited[0].1, "deny");
                    assert!(!audited[0].2);
                }
                PolicyMode::Audit => {
                    assert_eq!(
                        audited[0].1, "would_deny",
                        "audit must not read as 'allow' for the calls enforce would block"
                    );
                    assert!(audited[0].2, "audit does not block");
                }
                PolicyMode::Off => unreachable!(),
            }
        }
    }

    /// No session in `_meta` and no resolvable scope -- the state of every call
    /// before this phase. Guards the polarity: absence of the mechanism must
    /// narrow, never widen.
    #[tokio::test]
    async fn an_unresolvable_caller_cannot_decide_under_enforce() {
        let repo = Arc::new(StubDraftRepo::new());
        repo.save(pending_draft("d1", "sess-a", None))
            .await
            .unwrap();
        let auth = Arc::new(StubAuthority {
            mode: PolicyMode::Enforce,
            scope: None,
            audited: Mutex::new(Vec::new()),
        });
        let server =
            DraftMcpServer::new(repo.clone()).with_authority(Some(auth as Arc<dyn DraftAuthority>));

        let text = server
            .decide(&rmcp::model::Meta::new(), "d1", DraftStatus::Approved)
            .await;

        assert!(text.contains("not permitted"), "got: {text}");
        assert_eq!(
            repo.get("d1").await.unwrap().unwrap().status,
            DraftStatus::Pending
        );
    }

    /// `reject_draft` never called `get` at all, so it would flip an
    /// already-Approved draft to Rejected long after the fact.
    #[tokio::test]
    async fn reject_will_not_flip_a_draft_that_was_already_decided() {
        let repo = Arc::new(StubDraftRepo::new());
        repo.save(pending_draft("d1", "sess-a", Some("liz")))
            .await
            .unwrap();
        repo.update_status("d1", DraftStatus::Approved)
            .await
            .unwrap();
        let auth = Arc::new(StubAuthority {
            mode: PolicyMode::Enforce,
            scope: Some(ProfileScope::Owner("liz".into())),
            audited: Mutex::new(Vec::new()),
        });
        let server =
            DraftMcpServer::new(repo.clone()).with_authority(Some(auth as Arc<dyn DraftAuthority>));

        let text = server
            .decide(&meta_for("sess-a"), "d1", DraftStatus::Rejected)
            .await;

        assert!(text.contains("not pending"), "got: {text}");
        assert_eq!(
            repo.get("d1").await.unwrap().unwrap().status,
            DraftStatus::Approved,
            "an authorised action must not be un-authorised after the fact"
        );
    }

    /// The owner may still decide their own draft from a different session --
    /// otherwise the rule would be indistinguishable from "nobody may decide
    /// anything", which passes a deny test for the wrong reason.
    #[tokio::test]
    async fn the_owner_may_still_approve_their_own_draft_under_enforce() {
        let repo = Arc::new(StubDraftRepo::new());
        repo.save(pending_draft("d1", "sess-a", Some("liz")))
            .await
            .unwrap();
        let auth = Arc::new(StubAuthority {
            mode: PolicyMode::Enforce,
            scope: Some(ProfileScope::Owner("liz".into())),
            audited: Mutex::new(Vec::new()),
        });
        let server = DraftMcpServer::new(repo.clone())
            .with_authority(Some(auth.clone() as Arc<dyn DraftAuthority>));

        let text = server
            .decide(&meta_for("sess-b"), "d1", DraftStatus::Approved)
            .await;

        assert!(text.contains("approved"), "got: {text}");
        assert_eq!(
            repo.get("d1").await.unwrap().unwrap().status,
            DraftStatus::Approved
        );
        let audited = auth.audited.lock().unwrap().clone();
        assert_eq!(audited[0].0, "draft_approve");
        assert_eq!(audited[0].1, "allow");
    }

    /// `save_draft` must stamp the owner from the resolved scope. Without this
    /// every draft lands unowned in production and an ownership rule keyed on
    /// the column is a no-op that every deny test still passes.
    #[tokio::test]
    async fn save_draft_stamps_the_engine_session_and_the_resolved_owner() {
        let repo = Arc::new(StubDraftRepo::new());
        let auth = Arc::new(StubAuthority {
            mode: PolicyMode::Audit,
            scope: Some(ProfileScope::Owner("liz".into())),
            audited: Mutex::new(Vec::new()),
        });
        let server =
            DraftMcpServer::new(repo.clone()).with_authority(Some(auth as Arc<dyn DraftAuthority>));

        let (session, scope) = server.caller(&meta_for("20260805_7")).await;
        assert_eq!(session, "20260805_7");
        assert_eq!(scope.as_ref().and_then(|(s, _)| s.owner_id()), Some("liz"));
        // ...and the model's own parameter loses to it.
        assert_eq!(
            DraftMcpServer::scope_session(&session, Some("default")),
            "20260805_7"
        );
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

    // ── PAI-7 invariant 7 on the decision path ──────────────────────────────

    /// The expiry refusal must not depend on the policy layer: it is a property
    /// of the staged action, not of who is asking. Driven under the mode that
    /// permits everything (`Off` short-circuits before the ownership gate) and
    /// under the strictest one, with an owner who WOULD be allowed.
    #[tokio::test]
    async fn an_expired_draft_cannot_be_approved_in_any_policy_mode() {
        for mode in [PolicyMode::Off, PolicyMode::Audit, PolicyMode::Enforce] {
            let repo = Arc::new(StubDraftRepo::new());
            repo.save(expiring_draft("d1", "sess-a", Some("liz"), false))
                .await
                .unwrap();
            let auth = Arc::new(StubAuthority {
                mode,
                scope: Some(ProfileScope::Owner("liz".into())),
                audited: Mutex::new(Vec::new()),
            });
            let server = DraftMcpServer::new(repo.clone())
                .with_authority(Some(auth as Arc<dyn DraftAuthority>));

            let text = server
                .decide(&meta_for("sess-a"), "d1", DraftStatus::Approved)
                .await;

            assert!(
                text.contains("expired"),
                "mode {mode:?}: the refusal must name the defect, got: {text}"
            );
            assert_eq!(
                repo.get("d1").await.unwrap().unwrap().status,
                DraftStatus::Pending,
                "mode {mode:?}: an expired draft must not reach `approved`"
            );
        }
    }

    /// The vacuity control for the test above: the same owner, the same modes
    /// and a LIVE draft must approve. Without it, a `decide` that refused
    /// everything would pass the expiry test.
    #[tokio::test]
    async fn a_live_draft_with_an_expiry_still_approves() {
        for mode in [PolicyMode::Off, PolicyMode::Audit, PolicyMode::Enforce] {
            let repo = Arc::new(StubDraftRepo::new());
            repo.save(expiring_draft("d1", "sess-a", Some("liz"), true))
                .await
                .unwrap();
            let auth = Arc::new(StubAuthority {
                mode,
                scope: Some(ProfileScope::Owner("liz".into())),
                audited: Mutex::new(Vec::new()),
            });
            let server = DraftMcpServer::new(repo.clone())
                .with_authority(Some(auth as Arc<dyn DraftAuthority>));

            server
                .decide(&meta_for("sess-a"), "d1", DraftStatus::Approved)
                .await;
            assert_eq!(
                repo.get("d1").await.unwrap().unwrap().status,
                DraftStatus::Approved,
                "mode {mode:?}"
            );
        }
    }

    /// Rejecting an expired proposal must stay possible. PAI-7 section 3.5
    /// turns a rejection into a memory, which is how "we never want to be told
    /// about the garage door during the day" becomes a learned constraint. A
    /// guard that blocked both verbs would silently delete that feedback.
    #[tokio::test]
    async fn an_expired_draft_can_still_be_rejected() {
        let repo = Arc::new(StubDraftRepo::new());
        repo.save(expiring_draft("d1", "sess-a", Some("liz"), false))
            .await
            .unwrap();
        let auth = Arc::new(StubAuthority {
            mode: PolicyMode::Enforce,
            scope: Some(ProfileScope::Owner("liz".into())),
            audited: Mutex::new(Vec::new()),
        });
        let server =
            DraftMcpServer::new(repo.clone()).with_authority(Some(auth as Arc<dyn DraftAuthority>));

        server
            .decide(&meta_for("sess-a"), "d1", DraftStatus::Rejected)
            .await;
        assert_eq!(
            repo.get("d1").await.unwrap().unwrap().status,
            DraftStatus::Rejected,
            "the expiry guard must apply to approval only: a rejection an expired \
             proposal cannot receive is a memory PAI-7 3.5 never gets to write"
        );
    }
}
