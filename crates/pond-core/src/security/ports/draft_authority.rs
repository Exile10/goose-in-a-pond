//! Driven Port: who is deciding a draft, and how hard the policy bites.
//!
//! The draft MCP server is a process-wide singleton. Goose's `SpawnServerFn` is
//! `fn(DuplexStream, DuplexStream)` -- no session parameter, no capture -- and
//! `ExtensionManager::add_extension` early-returns for an unchanged config, so
//! the first session to load `giap-draft` spawns the only instance and every
//! later session reuses it. There is therefore no per-session state to hang a
//! speaker on, and the process-global `set_current_session_id` cannot be used:
//! `sse_semaphore` is `Semaphore::new(4)`, so four turns race that one cell and
//! using it would trade an authorisation hole for a misattribution bug.
//!
//! What a tool call DOES carry is the engine session id, per call, in the MCP
//! request `_meta` under `agent-session-id`. This port turns that id into the
//! things a decision needs. It lives in `pond-core` because the rule is policy;
//! the implementation is mechanism and needs three repositories.

use crate::security::ports::policy::{PolicyDecision, PolicyMode};
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::session::IdentificationSource;
use async_trait::async_trait;

#[async_trait]
pub trait DraftAuthority: Send + Sync {
    /// The mode in force right now. Read fresh on every decision, never cached:
    /// an operator flipping `security_policy_mode` is doing it because
    /// something is wrong, and a cached value would take effect at some
    /// unpredictable later point.
    async fn policy_mode(&self) -> PolicyMode;

    /// Who is speaking in the engine session this tool call came from.
    ///
    /// `None` means unresolvable -- an unmapped engine session, a storage
    /// error, or no session at all. Callers must treat `None` as a refusal,
    /// never as permission.
    async fn actor_for_engine_session(
        &self,
        engine_session_id: &str,
    ) -> Option<(ProfileScope, IdentificationSource)>;

    /// Record a decision at the boundary. Must never fail the caller.
    ///
    /// Takes the whole [`PolicyDecision`] rather than an `ok: bool` for the same
    /// reason [`SecurityPolicy::audit`] does: under `audit` the effect of a
    /// refusal is "permitted", so `ok` cannot answer what `enforce` would have
    /// blocked. `action` is a plain verb (`draft_approve`) — the verdict used to
    /// ride the action string as a `:{verdict}` suffix, and a report that has to
    /// parse a substring out of a free-form field is one rename from reading
    /// zero forever.
    ///
    /// [`SecurityPolicy::audit`]: crate::security::ports::policy::SecurityPolicy::audit
    async fn audit(&self, engine_session_id: &str, action: &str, decision: &PolicyDecision);
}
