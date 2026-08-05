//! Port definition for onboarding persistence.
//! This trait defines how onboarding state is stored and retrieved.

use crate::user_data::domain::onboarding::OnboardingStep;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait OnboardingRepository: Send + Sync {
    async fn get_current_step(&self) -> Option<OnboardingStep>;
    async fn save_step(&self, step: OnboardingStep) -> anyhow::Result<()>;
    /// Reset onboarding state to allow starting from scratch.
    async fn reset(&self) -> anyhow::Result<()>;

    /// Whether onboarding has been completed.
    ///
    /// Separate from [`Self::get_current_step`] because that method cannot
    /// tell "there is no row" from "I could not read the row": it returns
    /// `Option<OnboardingStep>`, and every consumer reads `None` as "not
    /// started". PAI-2 P7 keys the public-route allowlist on this answer, so
    /// that collapse would re-open every onboarding write hole on a fully
    /// set-up pond the moment SQLite returned `BUSY`. Returning a `Result`
    /// lets the caller narrow on failure instead of widening.
    ///
    /// **Deliberately has no default body.** A default would be a
    /// scope-widening default: an implementor that inherited it would answer
    /// from `get_current_step`, and a stub that answers "not onboarded" makes
    /// every onboarding write route public wherever it is used. PAI-1
    /// invariant 2 says access must NARROW on failure, and a default cannot
    /// know which way is narrow for the adapter it lands on. Required means
    /// the compiler names every implementor that has not decided.
    async fn is_complete(&self) -> anyhow::Result<bool>;
}

// Allows Arc<dyn OnboardingRepository> to be used wherever R: OnboardingRepository is required
#[async_trait::async_trait]
impl OnboardingRepository for Arc<dyn OnboardingRepository + Send + Sync> {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        self.as_ref().get_current_step().await
    }

    async fn save_step(&self, step: OnboardingStep) -> anyhow::Result<()> {
        self.as_ref().save_step(step).await
    }

    async fn reset(&self) -> anyhow::Result<()> {
        self.as_ref().reset().await
    }

    /// Forwarded, and it has to be.
    ///
    /// `AppState` holds an `Arc<dyn OnboardingRepository>`, so method
    /// resolution finds this impl before the concrete adapter. If
    /// `is_complete` ever gains a default body on the trait, deleting this arm
    /// stops being a compile error and starts being a silent behaviour change:
    /// the default would run here, the SQLite override would never execute,
    /// and nothing about the code would look wrong. That is why the trait
    /// method is required rather than defaulted.
    async fn is_complete(&self) -> anyhow::Result<bool> {
        self.as_ref().is_complete().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An adapter that can tell "no row" from "could not read" -- which the
    /// SQLite one can, and `get_current_step` has no way to express.
    struct UnreadableRepo;

    #[async_trait::async_trait]
    impl OnboardingRepository for UnreadableRepo {
        async fn get_current_step(&self) -> Option<OnboardingStep> {
            None
        }
        async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> {
            Ok(())
        }
        async fn reset(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn is_complete(&self) -> anyhow::Result<bool> {
            Err(anyhow::anyhow!("database is locked"))
        }
    }

    /// The guard for the forwarding arm above. A read failure must survive the
    /// `Arc`, because the alternative -- `Ok(false)`, "not onboarded" -- is the
    /// state in which PAI-2 P7 leaves every onboarding write route public.
    #[tokio::test]
    async fn a_read_failure_survives_the_arc_rather_than_becoming_not_onboarded() {
        let repo: Arc<dyn OnboardingRepository + Send + Sync> = Arc::new(UnreadableRepo);
        assert!(
            repo.is_complete().await.is_err(),
            "the Arc impl swallowed the read failure and answered 'not onboarded'"
        );
    }
}
