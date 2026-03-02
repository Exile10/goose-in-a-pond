use crate::domain::onboarding::OnboardingStep;

/// storage for onboarding steps
pub trait OnboardingRepository {
    /// Get the current onboarding step (None if not started)
    fn get_current_step(&self) -> Option<OnboardingStep>;

    /// Save/update the current onboarding step
    fn save_step(&self, step: OnboardingStep);
}