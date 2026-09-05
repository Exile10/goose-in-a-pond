//! InferencePool port — concurrent access to a shared LLM provider. Implementations control
//! concurrency with semaphores: HTTP providers (Ollama, llamafile) allow 3 or more concurrent
//! requests, while local GGUF serializes to 1.

use crate::models::domain::message::ChatMessage;
use anyhow::Result;
use async_trait::async_trait;

/// Priority levels for inference tasks.
///
/// Lower ordinal = higher priority. Used by the adapter to schedule
/// work when the backend serializes requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskPriority {
    /// Main chat — user is waiting for first token.
    Interactive = 0,
    /// Tool classification — gates the main chat prompt.
    Classifier = 1,
    /// Answer review — runs after main chat completes.
    Review = 2,
    /// Background tasks — memory extraction, consolidation.
    Background = 3,
}

/// Result of an inference pool submission.
pub struct InferenceResult {
    pub response: ChatMessage,
}

/// Driven port: submit LLM completion tasks for concurrent execution. The pool shares one
/// `LlmProvider` across every task, bounded by the adapter's semaphore: HTTP providers allow
/// parallel requests, GGUF serializes behind Goose's model mutex.
#[async_trait]
pub trait InferencePool: Send + Sync {
    /// Submit a completion request. Returns when inference completes.
    ///
    /// `task_id` is used for logging/tracing only.
    async fn submit(
        &self,
        task_id: &str,
        system_prompt: String,
        messages: Vec<ChatMessage>,
        priority: TaskPriority,
    ) -> Result<InferenceResult>;

    /// Maximum concurrent requests this pool supports.
    fn concurrency(&self) -> usize;
}
