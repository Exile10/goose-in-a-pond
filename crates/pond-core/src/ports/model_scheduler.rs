//! ModelScheduler port — driven port for memory-aware model lifecycle management.

use async_trait::async_trait;

/// Snapshot of the LLM memory budget on the current device.
#[derive(Debug, Clone, Default)]
pub struct MemoryStatus {
    /// Total device RAM in MB (all uses combined).
    pub total_mb: u64,
    /// MB currently available for LLM loading.
    pub available_for_llm_mb: u64,
    /// Name of the model currently loaded in the LLM slot, if any.
    pub loaded_model: Option<String>,
}

/// Driven port: manages model loading, eviction, and pre-loading hints.
///
/// The real adapter (`ResourceAwareModelScheduler`) uses Goose's
/// `InferenceRuntime` to query live memory and track which model is hot.
/// A `NoopScheduler` is used for llamafile / Ollama backends that manage
/// their own memory externally.
#[async_trait]
pub trait ModelScheduler: Send + Sync {
    /// Hint that the wake word was just detected — the scheduler may
    /// begin pre-loading the chat model in the background before the
    /// user has finished speaking.
    async fn notify_wake_word(&self);

    /// Return a snapshot of current memory usage.
    fn memory_status(&self) -> MemoryStatus;
}
