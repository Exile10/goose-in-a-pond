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
}
