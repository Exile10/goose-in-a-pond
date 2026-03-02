use crate::domain::onboarding::OnboardingStep;
use crate::ports::onboarding::OnboardingRepository;

/// Service to manage onboarding steps
pub struct OnboardingService<R: OnboardingRepository> {
    repo: R,
}

impl<R: OnboardingRepository> OnboardingService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }

    /// Start onboarding 
    pub fn start(&self) {
        self.repo.save_step(OnboardingStep::VerifyDevice);
    }

    /// Move to the next onboarding step
    pub fn advance(&self) {
        if let Some(current) = self.repo.get_current_step() {
            let next = current.next();
            self.repo.save_step(next);
        }
    }

    /// Get current onboarding step
    pub fn status(&self) -> Option<OnboardingStep> {
        self.repo.get_current_step()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::onboarding::OnboardingStep;
    use std::cell::RefCell;

    /// Mock repository for testing
    struct MockRepo {
        step: RefCell<Option<OnboardingStep>>,
    }

    impl MockRepo {
        fn new() -> Self {
            Self { step: RefCell::new(None) }
        }
    }

    impl crate::ports::onboarding::OnboardingRepository for MockRepo {
        fn get_current_step(&self) -> Option<OnboardingStep> { *self.step.borrow() }
        fn save_step(&self, step: OnboardingStep) { *self.step.borrow_mut() = Some(step); }
    }

    #[test]
    fn start_sets_initial_step() {
        let repo = MockRepo::new();
        let service = OnboardingService::new(repo);

        service.start();

        assert_eq!(service.status(), Some(OnboardingStep::VerifyDevice));
    }

    #[test]
    fn advance_moves_to_next_step() {
        let repo = MockRepo::new();
        let service = OnboardingService::new(repo);

        service.start();
        service.advance();

        assert_eq!(service.status(), Some(OnboardingStep::CreateProfile));
    }

    #[test]
    fn completed_is_terminal() {
        let repo = MockRepo::new();
        let service = OnboardingService::new(repo);

        service.repo.save_step(OnboardingStep::Completed);
        service.advance();

        assert_eq!(service.status(), Some(OnboardingStep::Completed));
    }
}