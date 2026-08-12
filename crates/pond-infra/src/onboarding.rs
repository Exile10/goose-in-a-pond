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

    /// The same read as `get_current_step`, minus the `.ok()??` that turns a
    /// database error into "not started".
    ///
    /// PAI-2 P7 keys the public-route allowlist on this answer: while the pond
    /// is not onboarded, `PUT /settings`, `POST /profiles`,
    /// `PATCH /profiles/{id}` and the `/onboard/*` writes answer callers with
    /// no token. Reporting an unreadable table as "not onboarded" would
    /// therefore re-open all of them on a fully set-up pond, for as long as the
    /// read kept failing. The error goes to the caller, which closes instead.
    async fn is_complete(&self) -> anyhow::Result<bool> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT current_step FROM onboarding_state WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(match row {
            Some((step,)) => matches!(
                OnboardingStep::from_str(step.as_str()),
                Ok(OnboardingStep::Completed)
            ),
            None => false,
        })
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

    /// PAI-2 P7. The whole reason the port has `is_complete` at all.
    ///
    /// `get_current_step` ends `.ok()??`, so an unreadable table is reported as
    /// "not started" -- and "not started" is the state in which the auth
    /// allowlist leaves every onboarding write route public. The precondition
    /// assertion below is what stops this test reading as vacuous: it proves
    /// the two methods genuinely disagree about the same broken database.
    #[tokio::test]
    async fn is_complete_reports_a_read_failure_instead_of_answering_not_onboarded() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let repo = SqlxOnboardingRepository::new(db.system.clone());

        repo.save_step(OnboardingStep::Completed).await.unwrap();
        assert!(repo.is_complete().await.unwrap());

        // Break the read the way a real failure would.
        sqlx::query("DROP TABLE onboarding_state")
            .execute(&db.system)
            .await
            .unwrap();

        assert!(
            repo.get_current_step().await.is_none(),
            "precondition: the old path reports an unreadable table as 'not started', \
             which is the widest possible answer to the question the auth gate asks"
        );
        assert!(
            repo.is_complete().await.is_err(),
            "an unreadable onboarding table must be an error, not 'not onboarded'"
        );
    }
}
