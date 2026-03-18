use pond_core::domain::onboarding::OnboardingStep;
use pond_core::ports::onboarding::OnboardingRepository;
use std::sync::Mutex;

pub struct InMemoryOnboardingRepo {
    step: Mutex<Option<OnboardingStep>>,
}

impl InMemoryOnboardingRepo {
    pub fn new() -> Self {
        Self {
            step: Mutex::new(None),
        }
    }
}

impl OnboardingRepository for InMemoryOnboardingRepo {
    fn get_current_step(&self) -> Option<OnboardingStep> {
        *self.step.lock().unwrap()
    }

    fn save_step(&self, step: OnboardingStep) {
        *self.step.lock().unwrap() = Some(step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_repo_has_no_step() {
        let repo = InMemoryOnboardingRepo::new();
        assert_eq!(repo.get_current_step(), None);
    }

    #[test]
    fn save_and_get_step() {
        let repo = InMemoryOnboardingRepo::new();
        repo.save_step(OnboardingStep::VerifyDevice);
        assert_eq!(repo.get_current_step(), Some(OnboardingStep::VerifyDevice));
    }

    #[test]
    fn overwrite_step() {
        let repo = InMemoryOnboardingRepo::new();
        repo.save_step(OnboardingStep::VerifyDevice);
        repo.save_step(OnboardingStep::CreateProfile);
        assert_eq!(repo.get_current_step(), Some(OnboardingStep::CreateProfile));
    }
}
