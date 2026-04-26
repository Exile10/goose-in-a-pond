//! Driven Port: VoiceOutput
//!
//! Abstracts text-to-speech output so the Speak state of the workflow loop
//! does not depend on any specific synthesis backend.
//!
//! Implementations:
//! - `PrintOutput` — prints to stdout (default; no audio hardware required)
//! - `PiperOutput` — spawns the Piper TTS subprocess and plays through speaker

use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: VoiceOutput
#[async_trait]
pub trait VoiceOutput: Send + Sync {
    /// Synthesise and deliver `text` (speak aloud or print).
    async fn speak(&self, text: &str) -> Result<()>;

    /// Start a soft ambient thinking tone that loops until `stop_thinking_tone()`.
    /// Called when the LLM is inferring so the user hears that the system is working.
    /// Default implementation is a no-op (for PrintOutput / tests).
    fn start_thinking_tone(&self) {}

    /// Stop the thinking tone. Idempotent — safe to call when no tone is playing.
    fn stop_thinking_tone(&self) {}
}
