//! ScheduleExecutor — driven port for executing scheduled task actions.
//!
//! The scheduler adapter calls this on each cron fire.  Implementations
//! dispatch to the LLM agent (`AgentPrompt`) or an HTTP webhook (`Webhook`).

use crate::user_data::domain::schedule::TaskKind;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait ScheduleExecutor: Send + Sync {
    /// Execute the action described by `kind` and return a result summary.
    async fn execute(&self, task_id: &str, kind: &TaskKind) -> Result<String>;
}
