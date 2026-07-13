use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::security::domain::event::{Event, EventQuery};

/// A single entry from the `event_log` table in `pond_logs.db`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: i64,
    pub timestamp: String,
    /// Severity: `"INFO"` | `"WARN"` | `"ERROR"`
    pub level: String,
    /// Component or subsystem that emitted the event (e.g. `"agent"`, `"pond-server"`).
    pub source: String,
    pub message: String,
    /// Optional JSON metadata blob.
    pub metadata: Option<String>,
}

#[async_trait]
pub trait EventLogRepository: Send + Sync {
    /// Return up to `limit` recent entries, optionally filtered by `level`.
    async fn list(&self, limit: u32, level: Option<&str>) -> Result<Vec<LogEntry>>;
    /// Append a new entry to the log.
    async fn insert(
        &self,
        level: &str,
        source: &str,
        message: &str,
        metadata: Option<&str>,
    ) -> Result<()>;
}

/// Driven Port: the unified, append-only event log (#108).
///
/// Supersedes the ad-hoc [`EventLogRepository`] shape (which the desktop Logs
/// screen still uses) by storing the correlatable, typed [`Event`] model that
/// the whole pipeline emits into. The durable SQLite adapter (Q2-32) and the
/// activity query API (Q2-37) build on this trait; the legacy repository above
/// is retained until those land and callers migrate.
#[async_trait]
pub trait EventLog: Send + Sync {
    /// Append a single event. Append-only — events are never mutated.
    async fn append(&self, event: Event) -> Result<()>;

    /// Return matching events, newest first, honoring `query.limit`.
    async fn query(&self, query: EventQuery) -> Result<Vec<Event>>;

    /// Delete every event matching `query` (the same filters as [`query`], but
    /// `limit` is ignored). Returns the number of rows removed. Powers
    /// sensitivity-aware retention pruning and the user "clear my activity"
    /// control (#117).
    async fn purge(&self, query: EventQuery) -> Result<u64>;
}
