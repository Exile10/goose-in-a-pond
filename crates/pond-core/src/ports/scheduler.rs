//! SchedulerPort — schedule recurring tasks via cron expressions.
//!
//! Allows GIAP to fire time-based automations (e.g. "every morning at 8 AM,
//! summarise overnight sensor readings").  The adapter lives in
//! `pond-infra-scheduler` (workspace-included, no Goose dep).

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A scheduled task persisted by the scheduler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub label: String,
    /// 6-field cron expression with leading seconds field
    /// (e.g. `"0 0 8 * * *"` = 08:00:00 daily).
    /// Format: `<sec> <min> <hour> <day-of-month> <month> <day-of-week>`
    pub cron: String,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    pub paused: bool,
    pub currently_running: bool,
    /// The JSON payload stored with this task (e.g. `{"prompt": "..."}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

/// Request payload for creating a new scheduled task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTaskRequest {
    pub id: String,
    pub label: String,
    pub cron: String,
    /// Arbitrary JSON payload delivered to the task executor on each fire.
    /// For `WebhookTaskExecutor`, must contain `"webhook_url"`.
    pub payload: serde_json::Value,
}

#[async_trait]
pub trait SchedulerPort: Send + Sync {
    async fn create_task(&self, req: CreateTaskRequest) -> Result<ScheduledTask>;
    async fn list_tasks(&self) -> Result<Vec<ScheduledTask>>;
    async fn delete_task(&self, id: &str) -> Result<()>;
    async fn pause_task(&self, id: &str) -> Result<()>;
    async fn resume_task(&self, id: &str) -> Result<()>;
    async fn run_now(&self, id: &str) -> Result<()>;
}
