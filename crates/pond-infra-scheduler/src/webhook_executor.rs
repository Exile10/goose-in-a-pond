//! Webhook task executor — POSTs the task payload to `payload["webhook_url"]`.

use anyhow::{bail, Result};
use async_trait::async_trait;

#[async_trait]
pub trait TaskExecutor: Send + Sync + 'static {
    /// Called on each scheduled fire with the task's stored payload.
    async fn execute(&self, task_id: &str, payload: serde_json::Value) -> Result<()>;
}

/// Sends a POST request to the URL stored in `payload["webhook_url"]`.
/// The full payload JSON is sent as the request body.
pub struct WebhookTaskExecutor {
    client: reqwest::Client,
}

impl WebhookTaskExecutor {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for WebhookTaskExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TaskExecutor for WebhookTaskExecutor {
    async fn execute(&self, task_id: &str, payload: serde_json::Value) -> Result<()> {
        let url = payload
            .get("webhook_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("task {task_id}: payload missing 'webhook_url'"))?
            .to_string();

        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("task {task_id}: webhook POST failed: {e}"))?;

        if !resp.status().is_success() {
            bail!("task {task_id}: webhook returned {}", resp.status());
        }

        tracing::debug!("task {task_id}: webhook fired → {url} ({})", resp.status());
        Ok(())
    }
}
