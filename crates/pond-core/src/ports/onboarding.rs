//! Port definition for onboarding persistence.
//! This trait defines how onboarding state is stored and retrieved.

use crate::domain::onboarding::OnboardingStep;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait OnboardingRepository: Send + Sync {
    /// Retrieve the current onboarding step.
    async fn get_current_step(&self) -> Option<OnboardingStep>;

    /// Persist the current onboarding step.
    async fn save_step(&self, step: OnboardingStep);
}

#[async_trait::async_trait]
impl OnboardingRepository for Arc<dyn OnboardingRepository> {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        self.as_ref().get_current_step().await
    }

    async fn save_step(&self, step: OnboardingStep) {
        self.as_ref().save_step(step).await
    }
}