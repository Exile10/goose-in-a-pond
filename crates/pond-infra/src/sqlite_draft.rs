//! SQLite-backed implementation of [`DraftRepository`].

use anyhow::Result;
use async_trait::async_trait;
use pond_core::domain::draft::{Draft, DraftStatus};
use pond_core::ports::draft::DraftRepository;
use sqlx::{Pool, Sqlite};

pub struct SqliteDraftRepository {
    pool: Pool<Sqlite>,
}

impl SqliteDraftRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DraftRepository for SqliteDraftRepository {
    async fn save(&self, draft: Draft) -> Result<()> {
        sqlx::query(
            "INSERT INTO drafts (id, session_id, kind, summary, payload, status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&draft.id)
        .bind(&draft.session_id)
        .bind(&draft.kind)
        .bind(&draft.summary)
        .bind(&draft.payload)
        .bind(draft.status.to_string())
        .bind(draft.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_pending(&self, session_id: &str) -> Result<Vec<Draft>> {
        let rows: Vec<(String, String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, session_id, kind, summary, payload, status, created_at \
             FROM drafts WHERE session_id = ? AND status = 'pending' \
             ORDER BY created_at DESC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(row_to_draft).collect()
    }

    async fn get(&self, id: &str) -> Result<Option<Draft>> {
        let row: Option<(String, String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, session_id, kind, summary, payload, status, created_at \
                 FROM drafts WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(Some(row_to_draft(r)?)),
            None => Ok(None),
        }
    }

    async fn update_status(&self, id: &str, status: DraftStatus) -> Result<()> {
        let result = sqlx::query("UPDATE drafts SET status = ? WHERE id = ?")
            .bind(status.to_string())
            .bind(id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            anyhow::bail!("draft not found: {id}");
        }
        Ok(())
    }
}

fn row_to_draft(
    (id, session_id, kind, summary, payload, status, created_at): (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ),
) -> Result<Draft> {
    let status: DraftStatus = status
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{}", e))?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());

    Ok(Draft {
        id,
        session_id,
        kind,
        summary,
        payload,
        status,
        created_at,
    })
}
