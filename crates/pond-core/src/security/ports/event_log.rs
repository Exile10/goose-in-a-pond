use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::security::domain::event::{Event, EventQuery};

/// A single row of drained tracing output, from the `event_log` table in
/// `pond_logs.db`.
///
/// The table name is historical: it predates the unified [`Event`] (#108) and
/// is kept because renaming a table is a destructive migration for no
/// functional gain. What it holds is operational logging, not domain events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalLogEntry {
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

/// Driven Port: the backing store for the operational log viewer.
///
/// Its one production writer is the tracing drain in `pond-server`
/// (`tracing_setup::LogDrainHandle::drain_into`), which mirrors INFO+ tracing
/// output into SQLite so `GET /api/v1/logs` can serve it. Nothing else should
/// write here.
///
/// **This is not the audit trail.** Anything a user could reasonably ask the
/// assistant about — what it did, what it touched, who paired — belongs in
/// [`EventLog`] below, which is correlatable and privacy-classified. See
/// [`crate::security::ports::audit`] for how to choose.
#[async_trait]
pub trait OperationalLogRepository: Send + Sync {
    /// Return up to `limit` recent entries, optionally filtered by `level`.
    async fn list(&self, limit: u32, level: Option<&str>) -> Result<Vec<OperationalLogEntry>>;
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
/// The authoritative record of what the assistant did: a correlatable, typed
/// [`Event`] carrying a session/trace id, structured attributes and a privacy
/// classification. The durable SQLite adapter (Q2-32), the activity query API
/// (Q2-37), sensitivity-aware retention (#117) and the audit MCP tools (#115)
/// all build on this trait.
///
/// Prefer this for anything semantic. [`OperationalLogRepository`] above is a
/// separate concern — drained tracing output for the Logs viewer — and is
/// deliberately kept out of this store so `GET /api/v1/activity` is not flooded
/// with log lines.
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
