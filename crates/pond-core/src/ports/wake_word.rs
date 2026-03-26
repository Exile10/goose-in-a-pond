//! Driving Port: WakeWordDetector
//!
//! Abstracts wake-word / activation detection so the Wait state of the
//! workflow loop does not depend on any specific detection backend.
//!
//! `wait_for_activation()` blocks until the user (or hardware) signals that
//! the assistant should start listening.  The caller transitions to the
//! Listen state once this returns `Ok(())`.
//!
//! Implementations:
//! - `InstantActivation` — returns immediately (keyboard / stdin mode, tests)
//! - `WhisperKeywordDetector` — polls the mic via whisper.cpp until the
//!   trigger phrase is heard (voice mode polyfill until a native KWS lib)
//! - (future) `PorcupineDetector` — Picovoice Porcupine native KWS

use anyhow::Result;
use async_trait::async_trait;

/// Driving Port: WakeWordDetector
#[async_trait]
pub trait WakeWordDetector: Send + Sync {
    /// Block until the assistant should activate.
    ///
    /// Returns `Ok(())` when activation is detected.
    /// Returns `Err` on unrecoverable hardware / IO failure.
    async fn wait_for_activation(&self) -> Result<()>;

    /// Short label shown in the Wait state (e.g. `"Say \"Goose\"..."`).
    fn activation_prompt(&self) -> &str {
        "Waiting for activation..."
    }
}
