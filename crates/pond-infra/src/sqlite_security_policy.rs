//! SQLite-backed [`SecurityPolicy`] adapter.
//!
//! Audit entries are appended to the unified event log (#108) as `Auth`
//! events, so a policy decision is correlatable with the rest of a session and
//! carries a privacy classification — rather than landing in the operational
//! log as a formatted message string.
//!
//! Authorization is still a hook, not a gate: [`SecurityPolicy::allow`] returns
//! `Ok(true)` for everything because no rules exist yet.
//!
//! **Nothing calls [`SecurityPolicy::audit`] in production yet.** Every call
//! site today is a test; the adapter is wired into `AppState` as the extension
//! point real authorization will use. Auth events that *are* recorded live
//! (device paired, pairing verify failed) come from the pairing path (#189),
//! not from here. Do not read the presence of this adapter as evidence that
//! cross-boundary calls are being audited.
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

use pond_core::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use pond_core::security::ports::event_log::EventLog;
use pond_core::security::ports::policy::{Principal, PrincipalKind, SecurityPolicy};

/// `action` recorded for the audit events this adapter appends.
const AUDIT_ACTION: &str = "security.audit";

/// [`SecurityPolicy`] that allows every access and records audits as `Auth`
/// events in the unified event log.
pub struct SqliteSecurityPolicy {
    event_log: Arc<dyn EventLog>,
}

impl SqliteSecurityPolicy {
    /// Wrap the unified event log as the audit sink for this policy.
    pub fn new(event_log: Arc<dyn EventLog>) -> Self {
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
        // Sensitive, not Internal: `remote_addr` and `token:<client_id>`
        // identify a specific device, so retention and export policy must treat
        // these as personal data.
        let mut event = Event::new(EventCategory::Auth, AUDIT_ACTION)
            .attr("principal", principal_label(principal))
            .attr("action", action)
            .attr("scope", scope)
            .attr("ok", ok)
            .sensitivity(PrivacySensitivity::Sensitive);
        if let Some(addr) = &principal.remote_addr {
            event = event.attr("remote_addr", addr.as_str());
        }

        // Auditing must never fail the caller; log and swallow any error.
        if let Err(e) = self.event_log.append(event).await {
            tracing::warn!(error = %e, "failed to write security audit entry");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::sqlite_event_log::SqliteEventLog;
    use pond_core::security::domain::event::EventQuery;
    use pond_core::security::ports::policy::scopes;
    use tempfile::tempdir;

    async fn make_policy() -> (SqliteSecurityPolicy, Arc<dyn EventLog>, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let event_log: Arc<dyn EventLog> = Arc::new(SqliteEventLog::new(db.logs));
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

    /// Audits land in the unified log as typed `Auth` events — the fields are
    /// queryable attributes, not substrings of a formatted message.
    #[tokio::test]
    async fn audit_appends_a_typed_auth_event() {
        let (policy, event_log, _tmp) = make_policy().await;
        let principal = Principal::token("abc").with_remote_addr("10.0.0.2:5000");

        policy.audit(&principal, "read", scopes::MEMORY, true).await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.category, EventCategory::Auth);
        assert_eq!(ev.action, AUDIT_ACTION);
        assert_eq!(ev.attributes.get("principal"), Some(&"token:abc".into()));
        assert_eq!(ev.attributes.get("action"), Some(&"read".into()));
        assert_eq!(ev.attributes.get("scope"), Some(&scopes::MEMORY.into()));
        assert_eq!(ev.attributes.get("ok"), Some(&true.into()));
        assert_eq!(
            ev.attributes.get("remote_addr"),
            Some(&"10.0.0.2:5000".into())
        );
    }

    /// The privacy claim: an audit event names a device (`remote_addr`, and the
    /// client id inside `token:<id>`), so it must never be classified below
    /// `Sensitive` or retention and export policy would treat personal data as
    /// ordinary telemetry.
    #[tokio::test]
    async fn audit_events_are_classified_sensitive() {
        let (policy, event_log, _tmp) = make_policy().await;

        policy
            .audit(
                &Principal::token("abc").with_remote_addr("10.0.0.2:5000"),
                "read",
                scopes::MEMORY,
                true,
            )
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(
            events[0].privacy_sensitivity,
            PrivacySensitivity::Sensitive,
            "audit events identify a device and must not be downgraded"
        );
    }

    /// The separation this adapter's move exists to create: an audit event is a
    /// domain record, so it belongs in the unified log and must NOT land in the
    /// operational log that backs `GET /api/v1/logs`. Both stores live in the
    /// same `pond_logs.db`, so nothing but the code keeps them apart.
    #[tokio::test]
    async fn audit_does_not_write_to_the_operational_log() {
        use crate::sqlite_event_log::SqliteOperationalLog;
        use pond_core::security::ports::event_log::OperationalLogRepository;

        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let policy = SqliteSecurityPolicy::new(Arc::new(SqliteEventLog::new(db.logs.clone())));
        let operational = SqliteOperationalLog::new(db.logs.clone());

        policy
            .audit(&Principal::token("abc"), "read", scopes::MEMORY, true)
            .await;

        let rows = operational.list(50, None).await.unwrap();
        assert!(
            rows.is_empty(),
            "audit events must not reach the operational log: {rows:?}"
        );
    }

    /// A caller with no remote address (loopback, internal) must not get an
    /// empty `remote_addr` attribute — absent is meaningfully different from
    /// blank when the field is used to identify a device.
    #[tokio::test]
    async fn audit_omits_remote_addr_when_there_is_none() {
        let (policy, event_log, _tmp) = make_policy().await;

        policy
            .audit(&Principal::loopback(), "read", scopes::MEMORY, true)
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(
            events[0].attributes.get("principal"),
            Some(&"loopback".into())
        );
        assert!(!events[0].attributes.contains_key("remote_addr"));
    }
}
