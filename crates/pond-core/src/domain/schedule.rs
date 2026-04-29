//! Schedule domain types — pure Rust, no external framework imports.
//!
//! Represents scheduled automations that fire on a cron cadence.
//! Each schedule carries a [`TaskKind`] that determines what happens
//! on each fire: send a prompt to the LLM agent, or POST a webhook.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a scheduled task does when it fires.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskKind {
    /// Send the prompt to the LLM agent and store the response.
    AgentPrompt { prompt: String },
    /// POST to an external webhook URL (backward compat).
    Webhook { webhook_url: String },
}

/// A scheduled automation persisted by the scheduler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub label: String,
    /// 6-field cron expression: `<sec> <min> <hour> <dom> <month> <dow>`
    pub cron: String,
    /// IANA timezone (e.g. `"Africa/Nairobi"`). Cron is evaluated in this zone.
    pub timezone: String,
    pub kind: TaskKind,
    pub paused: bool,
    pub currently_running: bool,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Status of a single scheduled execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
}

/// A single execution record for a scheduled task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRun {
    pub id: String,
    pub schedule_id: String,
    pub status: RunStatus,
    /// The agent's response text, or webhook status message.
    pub result: Option<String>,
    /// Error message if `status == Failed`.
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_kind_serde_round_trip_agent() {
        let kind = TaskKind::AgentPrompt {
            prompt: "Good morning briefing".into(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"agent_prompt\""));
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn task_kind_serde_round_trip_webhook() {
        let kind = TaskKind::Webhook {
            webhook_url: "https://example.com/hook".into(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"webhook\""));
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn run_status_serde() {
        let s = RunStatus::Completed;
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"completed\"");
        let back: RunStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, RunStatus::Completed);
    }
}
