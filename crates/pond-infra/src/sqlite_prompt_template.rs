//! SQLite-backed implementation of `PromptTemplateRepository`.
//!
//! Uses the `prompt_templates` table in `pond_system.db` (migration 0009).

use anyhow::Result;
use async_trait::async_trait;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::prompt_template::PromptTemplate;
use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;

pub struct SqlitePromptTemplateRepository {
    pool: Pool<Sqlite>,
}

impl SqlitePromptTemplateRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn row_to_template(row: &sqlx::sqlite::SqliteRow) -> Result<PromptTemplate> {
    Ok(PromptTemplate {
        name: row.try_get("name")?,
        content: row.try_get("content")?,
        description: row.try_get("description")?,
        is_system: row.try_get::<i64, _>("is_system")? != 0,
        updated_at: row.try_get("updated_at")?,
    })
}

#[async_trait]
impl PromptTemplateRepository for SqlitePromptTemplateRepository {
    async fn get(&self, name: &str) -> Result<Option<PromptTemplate>> {
        let rows = sqlx::query(
            "SELECT name, content, description, is_system, updated_at \
             FROM prompt_templates WHERE name = ?",
        )
        .bind(name)
        .fetch_all(&self.pool)
        .await?;

        rows.first().map(row_to_template).transpose()
    }

    async fn list(&self) -> Result<Vec<PromptTemplate>> {
        let rows = sqlx::query(
            "SELECT name, content, description, is_system, updated_at \
             FROM prompt_templates ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(row_to_template).collect()
    }

    async fn upsert(&self, template: &PromptTemplate) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO prompt_templates \
             (name, content, description, is_system, updated_at) \
             VALUES (?, ?, ?, ?, datetime('now'))",
        )
        .bind(&template.name)
        .bind(&template.content)
        .bind(&template.description)
        .bind(template.is_system as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn insert_if_absent(&self, template: &PromptTemplate) -> Result<bool> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO prompt_templates \
             (name, content, description, is_system, updated_at) \
             VALUES (?, ?, ?, ?, datetime('now'))",
        )
        .bind(&template.name)
        .bind(&template.content)
        .bind(&template.description)
        .bind(template.is_system as i64)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete(&self, name: &str) -> Result<()> {
        sqlx::query("DELETE FROM prompt_templates WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
