//! Driving Port: VoiceInput. Abstracts input acquisition so the workflow loop does not depend on
//! stdin, a microphone or a specific ASR backend. `listen()` blocks until a complete utterance is
//! available; `Ok(None)` means EOF, and the loop should then terminate cleanly.

use anyhow::Result;
use async_trait::async_trait;

/// Q2-26: signal emitted by `listen_with_speculative` before the final transcript is confirmed.
/// `Ready` carries a provisional transcript; `Invalidated` says speech resumed, so that transcript
/// covered too short a clip. Start downstream work on `Ready`, but discard it on `Invalidated`.
#[derive(Clone)]
pub enum SpeculativeSignal {
    Ready(String),
    Invalidated,
}

/// Driving Port: VoiceInput
#[async_trait]
pub trait VoiceInput: Send + Sync {
    /// Capture one utterance and return its text.
    ///
    /// Returns `Ok(None)` when the input stream is exhausted (EOF / device
    /// closed) — the caller should exit its loop cleanly.
    async fn listen(&self) -> Result<Option<String>>;

    /// Like `listen()`, but invokes `on_speculative` with provisional
    /// transcripts as they become available, before the final transcript is
    /// confirmed (Q2-26). Implementations that don't support the overlap
    /// just call `listen()` and never invoke the callback.
    async fn listen_with_speculative(
        &self,
        on_speculative: Box<dyn Fn(SpeculativeSignal) + Send + Sync>,
    ) -> Result<Option<String>> {
        let _ = on_speculative;
        self.listen().await
    }

    /// Short label shown in the terminal prompt before each capture.
    ///
    /// Stdin implementations return `"> "`.
    /// Voice implementations may return `"🎤 "` or similar.
    fn prompt(&self) -> &str {
        "> "
    }

    /// Pre-load captured audio that `listen()` should transcribe instead of recording a fresh
    /// clip. Call before `listen()` when the wake-word detector already caught the command in the
    /// same breath. The default is a no-op; implementors supporting hand-off override it.
    fn prime_with_captured(&self, _wav: Vec<u8>) {}
}
