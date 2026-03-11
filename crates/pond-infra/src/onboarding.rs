//! SQLx implementation of the OnboardingRepository port.
//! connects Core logic to SQLite persistence.

use pond_core::domain::onboarding::OnboardingStep;
use pond_core::ports::onboarding::OnboardingRepository;
use sqlx::{Pool, Sqlite};

pub struct SqlxOnboardingRepository {
    pool: Pool<Sqlite>,
}

impl SqlxOnboardingRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl OnboardingRepository for SqlxOnboardingRepository {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT current_step FROM onboarding_state WHERE id = 1"
        )
            .fetch_optional(&self.pool)
            .await
            .ok()??;

        match row.0.as_str() {
            "VerifyDevice" => Some(OnboardingStep::VerifyDevice),
            "CreateProfile" => Some(OnboardingStep::CreateProfile),
            "ConfigurePersonality" => Some(OnboardingStep::ConfigurePersonality),
            "ConnectDevices" => Some(OnboardingStep::ConnectDevices),
            "Completed" => Some(OnboardingStep::Completed),
            _ => None,
        }
    }

    async fn save_step(&self, step: OnboardingStep) {
        let step_str = format!("{:?}", step);

        sqlx::query(
            r#"
            INSERT INTO onboarding_state (id, current_step)
            VALUES (1, ?)
            ON CONFLICT(id)
            DO UPDATE SET
                current_step = excluded.current_step,
                updated_at = datetime('now')
            "#
        )
            .bind(step_str)
            .execute(&self.pool)
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    #[tokio::test]
    async fn onboarding_repo_persists_state() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();

        let repo = SqlxOnboardingRepository::new(db.system.clone());

        repo.save_step(OnboardingStep::VerifyDevice).await;

        let step = repo.get_current_step().await;

        assert_eq!(step, Some(OnboardingStep::VerifyDevice));
    }
}