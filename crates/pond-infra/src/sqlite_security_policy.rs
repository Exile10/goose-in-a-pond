//! SQLite-backed [`SecurityPolicy`] adapter.
//!
//! Wraps an [`EventLogRepository`] so that audit entries land in the
//! `event_log` table of `pond_logs.db`. Authorization is still a hook, not a
//! gate: [`SecurityPolicy::allow`] returns `Ok(true)` for everything because no
//! rules exist yet. Only [`SecurityPolicy::audit`] does real work here —
//! appending one INFO row per cross-boundary call.
//!
//! Only the event-log port is composed in. The [`Handshake`] port was
//! considered (it could feed token validation into `allow`), but with
//! default-allow there are no rules to evaluate, so taking the dependency now
//! would be dead weight. Token-scoped rules can wrap `Handshake` here when
//! real authorization lands.
//!
//! [`Handshake`]: pond_core::security::ports::handshake::Handshake

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use pond_core::security::ports::event_log::EventLogRepository;
use pond_core::security::ports::policy::{Principal, PrincipalKind, SecurityPolicy};

/// `source` column value for audit rows this adapter writes.
const AUDIT_SOURCE: &str = "security";

/// [`SecurityPolicy`] that allows every access and records audits to the
/// `event_log` table via an injected [`EventLogRepository`].
pub struct SqliteSecurityPolicy {
    event_log: Arc<dyn EventLogRepository>,
}

impl SqliteSecurityPolicy {
    /// Wrap an event-log repository as the audit sink for this policy.
    pub fn new(event_log: Arc<dyn EventLogRepository>) -> Self {
        Self { event_log }
    }
}

/// Render a [`Principal`] into a short, stable token for audit messages.
fn principal_label(principal: &Principal) -> String {
    match &principal.kind {
        PrincipalKind::Loopback => "loopback".to_string(),
        PrincipalKind::Internal => "internal".to_string(),
        PrincipalKind::Token(client_id) => format!("token:{client_id}"),
    }
}

#[async_trait]
impl SecurityPolicy for SqliteSecurityPolicy {
    async fn allow(&self, _principal: &Principal, _scope: &str) -> Result<bool> {
        // Hook, not a gate: no authorization rules exist yet.
        Ok(true)
    }

    async fn audit(&self, principal: &Principal, action: &str, scope: &str, ok: bool) {
        let who = principal_label(principal);
        let message = format!("{action} {scope} {who} ok={ok}");
        let metadata = serde_json::json!({
            "principal": who,
            "remote_addr": principal.remote_addr,
            "action": action,
            "scope": scope,
            "ok": ok,
        })
        .to_string();

        // Auditing must never fail the caller; log and swallow any error.
        if let Err(e) = self
            .event_log
            .insert("INFO", AUDIT_SOURCE, &message, Some(&metadata))
            .await
        {
            tracing::warn!(error = %e, "failed to write security audit entry");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::sqlite_event_log::SqliteEventLogRepository;
    use pond_core::security::ports::policy::scopes;
    use tempfile::tempdir;

    async fn make_policy() -> (
        SqliteSecurityPolicy,
        Arc<dyn EventLogRepository>,
        tempfile::TempDir,
    ) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let event_log: Arc<dyn EventLogRepository> =
            Arc::new(SqliteEventLogRepository::new(db.logs));
        let policy = SqliteSecurityPolicy::new(event_log.clone());
        (policy, event_log, tmp)
    }

    #[tokio::test]
    async fn allow_returns_true() {
        let (policy, _log, _tmp) = make_policy().await;
        assert!(policy
            .allow(&Principal::loopback(), scopes::MEMORY)
            .await
            .unwrap());
        assert!(policy
            .allow(&Principal::token("c1"), scopes::SECRETS)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn audit_writes_a_readable_row() {
        let (policy, event_log, _tmp) = make_policy().await;
        let principal = Principal::token("abc").with_remote_addr("10.0.0.2:5000");

        policy.audit(&principal, "read", scopes::MEMORY, true).await;

        let rows = event_log.list(10, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.level, "INFO");
        assert_eq!(row.source, AUDIT_SOURCE);
        assert!(row.message.contains("read"));
        assert!(row.message.contains("memory"));
        assert!(row.message.contains("token:abc"));
        assert!(row.message.contains("ok=true"));
        // Metadata carries the structured fields.
        let meta = row.metadata.as_ref().expect("metadata present");
        assert!(meta.contains("10.0.0.2:5000"));
    }
}
