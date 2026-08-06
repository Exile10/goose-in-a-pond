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
//! *Corrected 2026-08-06 (PAI-2 P8a).* This block used to say nothing called
//! [`SecurityPolicy::audit`] in production. That stopped being true when PAI-2
//! P1 landed the identity-assertion rule: `PUT /api/v1/sessions/{id}/user`
//! (`evaluate_identity_assertion` in `pond-api`) audits every decision through
//! this adapter, and `RepoDraftAuthority` audits every draft approve/reject.
//! Those two are still the only production call sites — the rest of the
//! cross-boundary surface is unaudited, so do not read the presence of this
//! adapter as evidence that everything is being recorded. Auth events for
//! pairing (device paired, verify failed) come from the pairing path (#189),
//! not from here.
//!
//! `GET /api/v1/security/policy-report` reads these events back, grouped on the
//! `verdict` attribute. That is the reason [`AUDIT_ACTION`] and the attribute
//! keys live in `pond-core` rather than here: writer and reader now share them,
//! and two private copies would have drifted into a report that answers zero.
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
use pond_core::security::ports::policy::{
    audit_attrs, PolicyDecision, Principal, PrincipalKind, SecurityPolicy, AUDIT_ACTION,
};

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

    async fn audit(
        &self,
        principal: &Principal,
        action: &str,
        scope: &str,
        decision: &PolicyDecision,
    ) {
        // Sensitive, not Internal: `remote_addr` and `token:<client_id>`
        // identify a specific device, so retention and export policy must treat
        // these as personal data.
        //
        // `verdict` is the field the policy report groups on. `ok` is kept
        // beside it rather than replaced by it -- they are different questions,
        // and under `audit` mode `ok` is `true` for every would-deny.
        let mut event = Event::new(EventCategory::Auth, AUDIT_ACTION)
            .attr(audit_attrs::PRINCIPAL, principal_label(principal))
            .attr(audit_attrs::ACTION, action)
            .attr(audit_attrs::SCOPE, scope)
            .attr(audit_attrs::OK, decision.allowed)
            .attr(audit_attrs::VERDICT, decision.verdict())
            .attr(audit_attrs::MODE, decision.mode.as_str())
            .sensitivity(PrivacySensitivity::Sensitive);
        if let Some(reason) = decision.denied_reason {
            // Absent rather than blank when there was nothing to refuse: an
            // empty string reads as "a reason we failed to record".
            event = event.attr(audit_attrs::REASON, reason);
        }
        if let Some(addr) = &principal.remote_addr {
            event = event.attr(audit_attrs::REMOTE_ADDR, addr.as_str());
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
    use pond_core::security::ports::policy::{scopes, PolicyMode, REASON_UNPROVEN_IDENTITY};
    use tempfile::tempdir;

    fn permit() -> PolicyDecision {
        PolicyDecision::permit(PolicyMode::Audit)
    }

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

        policy
            .audit(&principal, "read", scopes::MEMORY, &permit())
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.category, EventCategory::Auth);
        assert_eq!(ev.action, AUDIT_ACTION);
        assert_eq!(ev.attributes.get("principal"), Some(&"token:abc".into()));
        assert_eq!(ev.attributes.get("action"), Some(&"read".into()));
        assert_eq!(ev.attributes.get("scope"), Some(&scopes::MEMORY.into()));
        assert_eq!(ev.attributes.get("ok"), Some(&true.into()));
        assert_eq!(ev.attributes.get("verdict"), Some(&"allow".into()));
        assert_eq!(ev.attributes.get("mode"), Some(&"audit".into()));
        assert!(
            !ev.attributes.contains_key("reason"),
            "a permit has no reason; a blank one reads as a lost field"
        );
        assert_eq!(
            ev.attributes.get("remote_addr"),
            Some(&"10.0.0.2:5000".into())
        );
    }

    /// The whole reason the signature takes a decision rather than a bool. Both
    /// of these events carry `ok = true`; only `verdict` tells them apart, and
    /// only one of them is a call `enforce` would have blocked.
    #[tokio::test]
    async fn a_would_deny_is_distinguishable_from_an_allow_in_the_stored_event() {
        let (policy, event_log, _tmp) = make_policy().await;
        let principal = Principal::token("phone");

        policy
            .audit(&principal, "identify_session", scopes::SESSION, &permit())
            .await;
        policy
            .audit(
                &principal,
                "identify_session",
                scopes::SESSION,
                &PolicyDecision::refuse(PolicyMode::Audit, REASON_UNPROVEN_IDENTITY),
            )
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(events.len(), 2);
        let mut verdicts: Vec<_> = events
            .iter()
            .map(|e| e.attributes.get("verdict").cloned().unwrap())
            .collect();
        verdicts.sort_by_key(|v| format!("{v:?}"));
        assert_eq!(verdicts, vec!["allow".into(), "would_deny".into()]);
        assert!(
            events
                .iter()
                .all(|e| e.attributes.get("ok") == Some(&true.into())),
            "audit mode blocks nothing, so `ok` cannot be the discriminator"
        );
        let refused = events
            .iter()
            .find(|e| e.attributes.get("verdict") == Some(&"would_deny".into()))
            .unwrap();
        assert_eq!(
            refused.attributes.get("reason"),
            Some(&REASON_UNPROVEN_IDENTITY.into())
        );
    }

    /// An action string is a plain verb. The verdict used to ride it as a
    /// `:{verdict}` suffix; anything reading the report must read the attribute,
    /// so the string must not carry it back in.
    #[tokio::test]
    async fn the_action_string_does_not_carry_the_verdict() {
        let (policy, event_log, _tmp) = make_policy().await;
        policy
            .audit(
                &Principal::token("phone"),
                "identify_session",
                scopes::SESSION,
                &PolicyDecision::refuse(PolicyMode::Audit, REASON_UNPROVEN_IDENTITY),
            )
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(
            events[0].attributes.get("action"),
            Some(&"identify_session".into())
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
                &permit(),
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
            .audit(&Principal::token("abc"), "read", scopes::MEMORY, &permit())
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
            .audit(&Principal::loopback(), "read", scopes::MEMORY, &permit())
            .await;

        let events = event_log.query(EventQuery::default()).await.unwrap();
        assert_eq!(
            events[0].attributes.get("principal"),
            Some(&"loopback".into())
        );
        assert!(!events[0].attributes.contains_key("remote_addr"));
    }
}
