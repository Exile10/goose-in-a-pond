//! Memory-aware model scheduler for GIAP on resource-constrained devices.
//!
//! ## Design
//!
//! Only one LLM is resident in RAM at a time (Jetson Orin Nano, 8 GB unified).
//! The scheduler tracks which model is currently hot and signals wake-word
//! detection so a background task can begin pre-loading the chat model while
//! the user is still speaking.
//!
//! Goose's `InferenceRuntime` already implements evict-then-load: when
//! `LocalInferenceProvider::complete()` is called for a different model, it
//! sets all other model slots to `None` (RAII drop → llama.cpp free). The
//! scheduler wraps this with:
//! - Live memory reporting via `/proc/meminfo` (Linux/Jetson) or a zero fallback.
//! - A `tokio::sync::watch` channel so the server can spawn a pre-loader task.
//!
//! ## NoopScheduler
//!
//! For llamafile and Ollama backends, use `NoopScheduler`. Those providers
//! manage their own memory externally; GIAP does not control their eviction.

use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::watch;

use pond_core::ports::model_scheduler::{MemoryStatus, ModelScheduler};

// ── Jetson / Linux memory constants ──────────────────────────────────────────

/// Total device RAM on a Jetson Orin Nano 8 GB (MB).
pub const JETSON_TOTAL_RAM_MB: u64 = 8192;
/// Approximate headroom used by OS + GIAP server + UI at idle (MB).
const SYSTEM_OVERHEAD_MB: u64 = 1500;
/// Whisper base model resident size (MB).
const STT_RESERVED_MB: u64 = 200;
/// Piper / Qwen TTS resident size (MB).
const TTS_RESERVED_MB: u64 = 100;
/// Approximate MB available for a single LLM slot.
pub const LLM_BUDGET_MB: u64 =
    JETSON_TOTAL_RAM_MB - SYSTEM_OVERHEAD_MB - STT_RESERVED_MB - TTS_RESERVED_MB;

// ── ResourceAwareModelScheduler ──────────────────────────────────────────────

/// Scheduler for devices running local GGUF models (in-process llama.cpp).
///
/// Reads `/proc/meminfo` on Linux to report live memory. On other platforms
/// (macOS dev machines, CI) it falls back to the budget constants.
pub struct ResourceAwareModelScheduler {
    /// Name of the model currently loaded in the llama.cpp slot.
    currently_hot: Mutex<Option<String>>,
    /// Sending half — `notify_wake_word()` sends `true` on this channel.
    /// The server spawns a background task that receives and pre-loads.
    wake_tx: watch::Sender<bool>,
}

impl ResourceAwareModelScheduler {
    /// Create a new scheduler, also returning the receiving end of the
    /// wake-word channel so the server can spawn a pre-loader task.
    pub fn new() -> (Self, watch::Receiver<bool>) {
        let (wake_tx, wake_rx) = watch::channel(false);
        (
            Self {
                currently_hot: Mutex::new(None),
                wake_tx,
            },
            wake_rx,
        )
    }

    /// Update the currently-hot model name. Called by the server after a
    /// successful model load (optional — used for accurate reporting).
    pub fn set_hot_model(&self, name: Option<String>) {
        if let Ok(mut guard) = self.currently_hot.lock() {
            *guard = name;
        }
    }

    /// Read free RAM in MB from `/proc/meminfo` (Linux / Jetson).
    /// Returns `None` on non-Linux platforms or if the file cannot be read.
    fn read_free_ram_mb() -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            let content = std::fs::read_to_string("/proc/meminfo").ok()?;
            // "MemAvailable:   XXXXXX kB"
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("MemAvailable:") {
                    let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                    return Some(kb / 1024);
                }
            }
            None
        }
        #[cfg(not(target_os = "linux"))]
        None
    }
}

#[async_trait]
impl ModelScheduler for ResourceAwareModelScheduler {
    async fn notify_wake_word(&self) {
        // Signal receivers — the server's pre-loader task will start loading
        // the chat model. We ignore send errors (no receivers = no task running).
        let _ = self.wake_tx.send(true);
    }

    fn memory_status(&self) -> MemoryStatus {
        let loaded_model = self.currently_hot.lock().ok().and_then(|g| g.clone());

        let (total_mb, available_for_llm_mb) = match Self::read_free_ram_mb() {
            Some(free_mb) => (JETSON_TOTAL_RAM_MB, free_mb.min(LLM_BUDGET_MB)),
            None => {
                // Fallback: estimate based on whether a model is loaded.
                let used = if loaded_model.is_some() {
                    LLM_BUDGET_MB / 2 // rough midpoint
                } else {
                    0
                };
                (JETSON_TOTAL_RAM_MB, LLM_BUDGET_MB.saturating_sub(used))
            }
        };

        MemoryStatus {
            total_mb,
            available_for_llm_mb,
            loaded_model,
        }
    }
}

// ── NoopScheduler ────────────────────────────────────────────────────────────

/// Pass-through scheduler for llamafile and Ollama backends.
///
/// Those providers manage their own memory externally; GIAP does not evict
/// them. Memory status returns zeros so the UI shows "managed externally".
pub struct NoopScheduler;

#[async_trait]
impl ModelScheduler for NoopScheduler {
    async fn notify_wake_word(&self) {}

    fn memory_status(&self) -> MemoryStatus {
        MemoryStatus::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_scheduler_returns_zero_status() {
        let s = NoopScheduler;
        let status = s.memory_status();
        assert_eq!(status.total_mb, 0);
        assert_eq!(status.available_for_llm_mb, 0);
        assert!(status.loaded_model.is_none());
    }

    #[tokio::test]
    async fn resource_scheduler_tracks_hot_model() {
        let (sched, _rx) = ResourceAwareModelScheduler::new();
        assert!(sched.memory_status().loaded_model.is_none());

        sched.set_hot_model(Some("llama3.2-3b".to_string()));
        assert_eq!(
            sched.memory_status().loaded_model.as_deref(),
            Some("llama3.2-3b")
        );
    }

    #[tokio::test]
    async fn wake_word_sends_signal() {
        let (sched, mut rx) = ResourceAwareModelScheduler::new();

        // Initial value is false
        assert!(!*rx.borrow());

        sched.notify_wake_word().await;

        // After notification the channel has the new value
        rx.changed().await.unwrap();
        assert!(*rx.borrow());
    }

    #[test]
    fn llm_budget_is_positive_and_reasonable() {
        assert!(LLM_BUDGET_MB > 4096, "budget should be > 4GB on 8GB device");
        assert!(LLM_BUDGET_MB < JETSON_TOTAL_RAM_MB);
    }
}
