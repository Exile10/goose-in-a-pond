use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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
