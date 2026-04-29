//! SchedulerPort — schedule recurring tasks via cron expressions.
//!
//! Allows GIAP to fire time-based automations (e.g. "every morning at 8 AM,
//! summarise overnight sensor readings").  The adapter lives in
//! `pond-infra-scheduler` (workspace-included, no Goose dep).

use crate::domain::schedule::{Schedule, ScheduleRun, TaskKind};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// Request payload for creating a new scheduled task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreateScheduleRequest {
    pub id: String,
    pub label: String,
    /// 6-field cron expression with leading seconds field
    /// (e.g. `"0 0 8 * * *"` = 08:00:00 daily).
    pub cron: String,
    /// IANA timezone (e.g. `"Africa/Nairobi"`).
    pub timezone: String,
    /// What to do on each fire.
    pub kind: TaskKind,
}

#[async_trait]
pub trait SchedulerPort: Send + Sync {
    async fn create_task(&self, req: CreateScheduleRequest) -> Result<Schedule>;
    async fn list_tasks(&self) -> Result<Vec<Schedule>>;
    async fn delete_task(&self, id: &str) -> Result<()>;
    async fn pause_task(&self, id: &str) -> Result<()>;
    async fn resume_task(&self, id: &str) -> Result<()>;
    async fn run_now(&self, id: &str) -> Result<()>;

    /// Retrieve execution history for a schedule, most recent first.
    async fn get_runs(&self, schedule_id: &str, limit: u32) -> Result<Vec<ScheduleRun>>;

    /// List schedules sorted by next fire time (soonest first).
    async fn list_upcoming(&self, limit: u32) -> Result<Vec<Schedule>>;

    /// Inject the real executor after the agent is constructed.
    /// Called once during startup to break the circular init dependency.
    async fn set_executor(
        &self,
        executor: Arc<dyn crate::ports::schedule_execution::ScheduleExecutor>,
    ) -> Result<()>;
}

// ── Backward-compatible aliases ───���──────────────────────────────────────────
// These allow existing code that references the old names to keep compiling
// during the transition. Remove once all call sites are migrated.

/// Deprecated — use [`CreateScheduleRequest`] instead.
pub type CreateTaskRequest = CreateScheduleRequest;

/// Deprecated — use [`Schedule`] instead.
pub type ScheduledTask = Schedule;
