//! TokioInferencePool — concurrent LLM task submission backed by tokio tasks.
//!
//! Wraps the live `LlmProvider` (from the hot-swap `RwLock`) and bounds
//! concurrency with a semaphore. Provider-aware defaults:
//!
//! - **Ollama / llamafile** (HTTP servers): 3 concurrent requests.
//!   The server handles GPU serialization internally.
//! - **Local GGUF**: 1 concurrent request. Goose's model mutex serializes
//!   anyway, but the semaphore avoids queueing multiple blocked tasks.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::domain::message::ChatMessage;
use pond_core::ports::inference_pool::{InferencePool, InferenceResult, TaskPriority};
use pond_core::ports::provider::LlmProvider;
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};

pub struct TokioInferencePool {
    live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
    semaphore: Arc<Semaphore>,
    max_concurrency: usize,
}

impl TokioInferencePool {
    /// Create a pool with the given concurrency limit.
    pub fn new(
        live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
        max_concurrency: usize,
    ) -> Self {
        Self {
            live_provider,
            semaphore: Arc::new(Semaphore::new(max_concurrency)),
            max_concurrency,
        }
    }

    /// Create a pool with concurrency based on provider type.
    ///
    /// - `"ollama"` / `"llamafile"`: 3 concurrent (HTTP parallelism)
    /// - `"local"` / `"gguf"` / anything else: 1 (serialized)
    pub fn for_provider(
        live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
        provider_type: &str,
    ) -> Self {
        let max = match provider_type {
            "ollama" | "llamafile" => 3,
            _ => 1,
        };
        Self::new(live_provider, max)
    }
}

#[async_trait]
impl InferencePool for TokioInferencePool {
    async fn submit(
        &self,
        task_id: &str,
        system_prompt: String,
        messages: Vec<ChatMessage>,
        priority: TaskPriority,
    ) -> Result<InferenceResult> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| anyhow!("inference pool semaphore closed"))?;

        tracing::debug!(
            task = task_id,
            priority = ?priority,
            concurrency = self.max_concurrency,
            "inference pool: acquired permit"
        );

        let provider = {
            let guard = self.live_provider.read().await;
            guard
                .as_ref()
                .cloned()
                .ok_or_else(|| anyhow!("no LLM provider available"))?
        };

        let response = provider.complete(&system_prompt, messages).await?;

        Ok(InferenceResult { response })
    }

    fn concurrency(&self) -> usize {
        self.max_concurrency
    }
}
