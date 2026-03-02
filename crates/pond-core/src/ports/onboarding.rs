use crate::domain::onboarding::OnboardingStep;

pub trait OnboardingRepository {
    fn get_current_step(&self) -> Option<OnboardingStep>;
    fn save_step(&self, step: OnboardingStep);
}