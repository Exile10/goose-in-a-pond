//! Schedule executors — bridge between the scheduler infrastructure and the
//! agent / webhook execution targets.
//!
//! `AgentScheduleExecutor` sends prompts to the LLM agent and collects the
//! response.  `DeferredExecutor` wraps a `OnceCell` so the scheduler can be
//! constructed before the agent exists (breaking the circular init dependency).

use anyhow::{bail, Result};
use async_trait::async_trait;
use pond_core::domain::agent::AgentRequest;
use pond_core::domain::schedule::TaskKind;
use pond_core::ports::agent::Agent;
use pond_core::ports::schedule_execution::ScheduleExecutor;
use pond_core::ports::session_storage::SessionStorage;
use std::sync::Arc;
use tokio::sync::{OnceCell, Semaphore};

// ── AgentScheduleExecutor ────────────────────────────────────────────────────

/// Executes scheduled tasks by dispatching to the LLM agent or HTTP webhook.
pub struct AgentScheduleExecutor {
    agent: Arc<dyn Agent>,
    session_storage: Arc<dyn SessionStorage>,
    http_client: reqwest::Client,
    /// Limit concurrent scheduled runs to avoid starving interactive chat.
    semaphore: Semaphore,
}

impl AgentScheduleExecutor {
    pub fn new(
        agent: Arc<dyn Agent>,
        session_storage: Arc<dyn SessionStorage>,
        max_concurrent: u32,
    ) -> Self {
        Self {
            agent,
            session_storage,
            http_client: reqwest::Client::new(),
            semaphore: Semaphore::new(max_concurrent.max(1) as usize),
        }
    }
}

#[async_trait]
impl ScheduleExecutor for AgentScheduleExecutor {
    async fn execute(&self, task_id: &str, kind: &TaskKind) -> Result<String> {
        let _permit = self.semaphore.acquire().await?;

        match kind {
            TaskKind::AgentPrompt { prompt } => {
                let session_id = format!("sched-{}-{}", task_id, chrono::Utc::now().timestamp());
                // Create an ephemeral session for this run.
                let _ = self
                    .session_storage
                    .create_session(session_id.clone())
                    .await;

                let request = AgentRequest {
                    message: prompt.clone(),
                    session_id: session_id.clone(),
                    model_role: "task".to_string(),
                    images: vec![],
                    voice_mode: false,
                };

                tracing::info!("[scheduler] executing prompt for task {task_id}");
                let response = self.agent.chat(request).await?;
                tracing::info!(
                    "[scheduler] task {task_id} completed ({} chars)",
                    response.text.len()
                );
                Ok(response.text)
            }
            TaskKind::Webhook { webhook_url } => {
                tracing::info!("[scheduler] firing webhook for task {task_id}: {webhook_url}");
                let resp = self
                    .http_client
                    .post(webhook_url)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("task {task_id}: webhook POST failed: {e}"))?;

                let status = resp.status();
                if !status.is_success() {
                    bail!("task {task_id}: webhook returned {status}");
                }
                Ok(format!("Webhook returned {status}"))
            }
        }
    }
}

// ── DeferredExecutor ─────────────────────────────────────────────────────────

/// A `ScheduleExecutor` backed by a `OnceCell`.
///
/// Created empty during startup, then filled with a real executor after the
/// agent is constructed. This breaks the circular init dependency:
///
/// ```text
/// deferred_exec = DeferredExecutor::new()       // empty
/// scheduler     = CronSchedulerAdapter::new(..., deferred_exec.clone())
/// agent         = build_goose_backend(..., scheduler.clone(), ...)
/// real_exec     = AgentScheduleExecutor::new(agent, session_storage)
/// deferred_exec.init(real_exec)                  // ← filled
/// ```
pub struct DeferredExecutor {
    inner: OnceCell<Arc<dyn ScheduleExecutor>>,
}

impl DeferredExecutor {
    pub fn new() -> Self {
        Self {
            inner: OnceCell::new(),
        }
    }

    /// Fill the executor.  May only be called once; subsequent calls are no-ops.
    pub async fn init(&self, executor: Arc<dyn ScheduleExecutor>) {
        let _ = self.inner.set(executor);
    }
}

#[async_trait]
impl ScheduleExecutor for DeferredExecutor {
    async fn execute(&self, task_id: &str, kind: &TaskKind) -> Result<String> {
        match self.inner.get() {
            Some(exec) => exec.execute(task_id, kind).await,
            None => bail!(
                "task {task_id}: schedule executor not yet initialized \
                 (server is still starting up)"
            ),
        }
    }
}
