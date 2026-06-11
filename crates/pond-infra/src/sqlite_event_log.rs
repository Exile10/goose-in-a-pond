//! SQLite-backed implementation of `EventLogRepository`.
//!
//! Uses the `event_log` table in `pond_logs.db` (migration 0001).

use anyhow::Result;
use async_trait::async_trait;
use sqlx::{Pool, Row, Sqlite};

use pond_core::security::ports::event_log::{EventLogRepository, LogEntry};

pub struct SqliteEventLogRepository {
    pool: Pool<Sqlite>,
}

impl SqliteEventLogRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl EventLogRepository for SqliteEventLogRepository {
    async fn list(&self, limit: u32, level: Option<&str>) -> Result<Vec<LogEntry>> {
        let rows = match level {
            Some(lvl) => {
                sqlx::query(
                    "SELECT id, timestamp, level, source, message, metadata \
                     FROM event_log \
                     WHERE level = ? \
                     ORDER BY id DESC LIMIT ?",
                )
                .bind(lvl)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, timestamp, level, source, message, metadata \
                     FROM event_log \
                     ORDER BY id DESC LIMIT ?",
                )
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
        };

        Ok(rows
            .iter()
            .map(|r| LogEntry {
                id: r.get("id"),
                timestamp: r.get("timestamp"),
                level: r.get("level"),
                source: r.get("source"),
                message: r.get("message"),
                metadata: r.get("metadata"),
            })
            .collect())
    }

    async fn insert(
        &self,
        level: &str,
        source: &str,
        message: &str,
        metadata: Option<&str>,
    ) -> Result<()> {
        sqlx::query("INSERT INTO event_log (level, source, message, metadata) VALUES (?, ?, ?, ?)")
            .bind(level)
            .bind(source)
            .bind(message)
            .bind(metadata)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
