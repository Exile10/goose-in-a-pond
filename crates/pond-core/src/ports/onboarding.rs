//! Port definition for onboarding persistence.
//! This trait defines how onboarding state is stored and retrieved.

use crate::domain::onboarding::OnboardingStep;

#[async_trait::async_trait]
pub trait OnboardingRepository: Send + Sync {
    /// Retrieve the current onboarding step.
    async fn get_current_step(&self) -> Option<OnboardingStep>;

    /// Persist the current onboarding step.
    async fn save_step(&self, step: OnboardingStep);
}