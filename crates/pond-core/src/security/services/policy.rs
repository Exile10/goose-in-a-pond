//! Default [`SecurityPolicy`] implementation: allow everything.
//!
//! This is the production default while the privacy boundary is a hook and not
//! a gate. [`AllowAllPolicy::allow`] returns `Ok(true)` for every principal and
//! scope; [`AllowAllPolicy::audit`] emits a `tracing` debug line and keeps no
//! state. Deployments that want a real audit trail (e.g. one persisting to
//! `pond_logs.db`) swap in a pond-infra adapter without changing any consumer.

use crate::security::ports::policy::{Principal, SecurityPolicy};
use anyhow::Result;
use async_trait::async_trait;

/// Permissive [`SecurityPolicy`]: every access is allowed; audit is a no-op
/// beyond a `tracing::debug!` line.
#[derive(Debug, Default, Clone)]
pub struct AllowAllPolicy;

impl AllowAllPolicy {
    /// Construct the default allow-all policy.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SecurityPolicy for AllowAllPolicy {
    async fn allow(&self, _principal: &Principal, _scope: &str) -> Result<bool> {
        Ok(true)
    }

    async fn audit(&self, principal: &Principal, action: &str, scope: &str, ok: bool) {
        tracing::debug!(
            ?principal,
            action,
            scope,
            ok,
            "security audit (allow-all policy)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::ports::policy::{scopes, PrincipalKind};

    #[tokio::test]
    async fn allow_returns_true_for_every_principal_kind() {
        let policy = AllowAllPolicy::new();
        let principals = [
            Principal::loopback(),
            Principal::internal(),
            Principal::token("client-abc"),
        ];
        for principal in principals {
            assert!(policy.allow(&principal, scopes::MEMORY).await.unwrap());
            assert!(policy.allow(&principal, scopes::SECRETS).await.unwrap());
        }
    }

    #[tokio::test]
    async fn allow_returns_true_for_unknown_scope() {
        let policy = AllowAllPolicy::default();
        assert!(policy
            .allow(&Principal::loopback(), "some-future-scope")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn audit_does_not_panic() {
        let policy = AllowAllPolicy::new();
        let principal = Principal::token("client-xyz").with_remote_addr("10.0.0.5:55123");
        // Should complete without panicking for both outcomes.
        policy
            .audit(&principal, "read", scopes::PROFILE, true)
            .await;
        policy
            .audit(&Principal::internal(), "write", scopes::SCHEDULE, false)
            .await;
    }

    #[test]
    fn principal_constructors_set_expected_kind() {
        assert_eq!(Principal::loopback().kind, PrincipalKind::Loopback);
        assert_eq!(Principal::internal().kind, PrincipalKind::Internal);
        assert_eq!(
            Principal::token("c1").kind,
            PrincipalKind::Token("c1".to_string())
        );
    }
}
