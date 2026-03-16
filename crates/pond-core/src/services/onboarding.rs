//! Application service for onboarding use cases.
//! This orchestrates onboarding progression logic.

use crate::domain::onboarding::OnboardingStep;
use crate::ports::onboarding::OnboardingRepository;

pub struct OnboardingService<R: OnboardingRepository> {
    repo: R,
}

impl<R: OnboardingRepository> OnboardingService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }

    /// Start onboarding at the first step.
    pub async fn start(&self) {
        self.repo
            .save_step(OnboardingStep::VerifyDevice)
            .await;
    }

    /// Advance onboarding to next step.
    pub async fn advance(&self) {
        if let Some(current) = self.repo.get_current_step().await {
            let next = current.next();
            self.repo.save_step(next).await;
        }
    }

    /// Get the current onboarding status.
    pub async fn status(&self) -> Option<OnboardingStep> {
        self.repo.get_current_step().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockRepo {
        step: Mutex<Option<OnboardingStep>>,
    }

    impl MockRepo {
        fn new() -> Self {
            Self {
                step: Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl OnboardingRepository for MockRepo {
        async fn get_current_step(&self) -> Option<OnboardingStep> {
            *self.step.lock().unwrap()
        }

        async fn save_step(&self, step: OnboardingStep) {
            *self.step.lock().unwrap() = Some(step);
        }
    }

    #[tokio::test]
    async fn start_sets_initial_step() {
        let repo = MockRepo::new();
        let service = OnboardingService::new(repo);

        service.start().await;

        assert_eq!(service.status().await, Some(OnboardingStep::VerifyDevice));
    }

    #[tokio::test]
    async fn status_is_none_before_start() {
        let service = OnboardingService::new(MockRepo::new());
        assert_eq!(service.status().await, None);
    }

    #[tokio::test]
    async fn advance_does_nothing_before_start() {
        let service = OnboardingService::new(MockRepo::new());
        service.advance().await;
        assert_eq!(service.status().await, None);
    }

    #[tokio::test]
    async fn advance_moves_to_next_step() {
        let service = OnboardingService::new(MockRepo::new());
        service.start().await;
        service.advance().await;
        assert_eq!(service.status().await, Some(OnboardingStep::CreateProfile));
    }

    #[tokio::test]
    async fn advance_through_all_steps() {
        let service = OnboardingService::new(MockRepo::new());
        service.start().await;
        assert_eq!(service.status().await, Some(OnboardingStep::VerifyDevice));
        service.advance().await;
        assert_eq!(service.status().await, Some(OnboardingStep::CreateProfile));
        service.advance().await;
        assert_eq!(service.status().await, Some(OnboardingStep::ConfigurePersonality));
        service.advance().await;
        assert_eq!(service.status().await, Some(OnboardingStep::ConnectDevices));
        service.advance().await;
        assert_eq!(service.status().await, Some(OnboardingStep::Completed));
    }

    #[tokio::test]
    async fn advance_stays_at_completed() {
        let service = OnboardingService::new(MockRepo::new());
        service.start().await;
        for _ in 0..10 {
            service.advance().await;
        }
        assert_eq!(service.status().await, Some(OnboardingStep::Completed));
    }
}