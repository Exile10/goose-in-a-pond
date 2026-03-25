//! Driving Port: VoiceInput
//!
//! Abstracts text/voice input acquisition so the workflow loop does not
//! depend directly on stdin, a microphone, or any specific ASR backend.
//!
//! `listen()` blocks until a complete utterance is available, then returns
//! the transcribed text.  Returns `Ok(None)` on EOF / end-of-stream to
//! signal that the loop should terminate cleanly.

use anyhow::Result;
use async_trait::async_trait;

/// Driving Port: VoiceInput
#[async_trait]
pub trait VoiceInput: Send + Sync {
    /// Capture one utterance and return its text.
    ///
    /// Returns `Ok(None)` when the input stream is exhausted (EOF / device
    /// closed) — the caller should exit its loop cleanly.
    async fn listen(&self) -> Result<Option<String>>;

    /// Short label shown in the terminal prompt before each capture.
    ///
    /// Stdin implementations return `"> "`.
    /// Voice implementations may return `"🎤 "` or similar.
    fn prompt(&self) -> &str {
        "> "
    }
}
