//! Database initialization for Goose In A Pond
//!
//! GIAP uses two SQLite databases:
//! 1. **System DB** (`pond_system.db`) — Core application data (users,
//!    devices, sessions, onboarding state, settings)
//! 2. **Log DB** (`pond_logs.db`) — Logging, telemetry, audit trail
//!
//! # TODO
//! - [ ] Define the system DB schema (user will provide)
//! - [ ] Define the log DB schema (events, metrics, audit)
//! - [ ] Add migration support (sqlx::migrate!)
//! - [ ] Add connection pooling configuration
//! - [ ] Add backup/restore utilities
//! - [ ] Add database health check endpoint

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};
use std::path::Path;
use std::str::FromStr;

/// Holds connection pools for both GIAP databases.
pub struct Database {
    /// Core application data — users, devices, sessions, settings
    pub system: Pool<Sqlite>,
    /// Logging, telemetry, and audit trail
    pub logs: Pool<Sqlite>,
}

impl Database {
    /// Initialize both databases at the given directory.
    ///
    /// Creates the files if they don't exist and runs migrations.
    pub async fn init(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;

        let system_path = data_dir.join("pond_system.db");
        let logs_path = data_dir.join("pond_logs.db");

        let system = Self::connect(&system_path).await?;
        let logs = Self::connect(&logs_path).await?;

        // Run initial table creation
        Self::init_system_tables(&system).await?;
        Self::init_log_tables(&logs).await?;

        tracing::info!(
            "Databases initialized at {}",
            data_dir.display()
        );

        Ok(Self { system, logs })
    }

    async fn connect(path: &Path) -> Result<Pool<Sqlite>> {
        let opts = SqliteConnectOptions::from_str(
            &format!("sqlite:{}?mode=rwc", path.display()),
        )?
        .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;

        Ok(pool)
    }

    /// System DB schema initialization.
    ///
    /// TODO: Replace with user-provided schema.
    /// TODO: Move to sqlx migrations once schema is finalized.
    async fn init_system_tables(pool: &Pool<Sqlite>) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS _schema_version (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- TODO: User will provide the full system schema.
            -- Placeholder tables below:

            CREATE TABLE IF NOT EXISTS devices (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                hostname TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "#,
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Log DB schema initialization.
    async fn init_log_tables(pool: &Pool<Sqlite>) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS event_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL DEFAULT (datetime('now')),
                level TEXT NOT NULL DEFAULT 'INFO',
                source TEXT NOT NULL,
                message TEXT NOT NULL,
                metadata TEXT
            );

            CREATE TABLE IF NOT EXISTS system_info (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL DEFAULT (datetime('now')),
                key TEXT NOT NULL,
                value TEXT NOT NULL
            );
            "#,
        )
        .execute(pool)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn database_initializes() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();

        // Verify system db has tables
        let row: (i64,) = sqlx::query_as("SELECT count(*) FROM sqlite_master WHERE type='table'")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert!(row.0 >= 2, "Expected at least 2 tables in system db");

        // Verify logs db has tables
        let row: (i64,) = sqlx::query_as("SELECT count(*) FROM sqlite_master WHERE type='table'")
            .fetch_one(&db.logs)
            .await
            .unwrap();
        assert!(row.0 >= 2, "Expected at least 2 tables in logs db");
    }
}
