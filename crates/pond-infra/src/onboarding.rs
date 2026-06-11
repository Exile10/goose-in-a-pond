//! SQLx implementation of the OnboardingRepository port.
//! Connects Core logic to SQLite persistence.

use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use sqlx::{Pool, Sqlite};
use std::str::FromStr;

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
            "SELECT current_step FROM onboarding_state WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .ok()??;

        OnboardingStep::from_str(row.0.as_str()).ok()
    }

    async fn save_step(&self, step: OnboardingStep) -> anyhow::Result<()> {
        let step_str = step.to_string();

        sqlx::query(
            r#"
            INSERT INTO onboarding_state (id, current_step)
            VALUES (1, ?)
            ON CONFLICT(id)
            DO UPDATE SET
                current_step = excluded.current_step,
                updated_at = datetime('now')
            "#,
        )
        .bind(step_str)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn reset(&self) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM onboarding_state WHERE id = 1")
            .execute(&self.pool)
            .await?;
        Ok(())
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

        repo.save_step(OnboardingStep::Basics).await.unwrap();

        let step = repo.get_current_step().await;

        assert_eq!(step, Some(OnboardingStep::Basics));
    }
}
