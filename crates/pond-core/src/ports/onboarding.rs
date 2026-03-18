//! Port definition for onboarding persistence.
//! This trait defines how onboarding state is stored and retrieved.

use crate::domain::onboarding::OnboardingStep;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait OnboardingRepository: Send + Sync {
    async fn get_current_step(&self) -> Option<OnboardingStep>;
    async fn save_step(&self, step: OnboardingStep) -> anyhow::Result<()>;
    /// Reset onboarding state to allow starting from scratch.
    async fn reset(&self) -> anyhow::Result<()>;
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
}
