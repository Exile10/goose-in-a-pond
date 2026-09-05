//! Driven Port: VoiceOutput. Abstracts text-to-speech so the Speak state of the workflow loop
//! does not depend on a synthesis backend. `PrintOutput` prints to stdout (the default, needing
//! no audio hardware); `PiperOutput` spawns the Piper subprocess and plays through the speaker.

use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: VoiceOutput
#[async_trait]
pub trait VoiceOutput: Send + Sync {
    /// Synthesise and deliver `text` (speak aloud or print).
    async fn speak(&self, text: &str) -> Result<()>;

    /// Synthesize text to audio bytes WITHOUT playing, so the next sentence can be prepared while
    /// the current one plays. `None` means split synthesis is unsupported and the chat loop falls
    /// back to `speak()`.
    async fn synthesize(&self, _text: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Play pre-synthesized audio bytes (from `synthesize()`).
    /// Blocks until playback finishes.
    async fn play_audio(&self, _audio: Vec<u8>) -> Result<()> {
        Ok(())
    }

    /// Mark the start of one turn's speech, clearing any prior interrupt. Interrupt state belongs
    /// to the TURN, not one utterance: clear it per utterance and a barge-in during sentence one
    /// is forgotten by sentence two. Call once before the turn's first `speak()`/`play_audio()`.
    /// Idempotent; the default is a no-op (PrintOutput, tests).
    fn begin_utterance(&self) {}

    /// Immediately stop any in-progress speech playback.
    ///
    /// Called when the user interrupts with the wake word during TTS output.
    /// Idempotent — safe to call when nothing is playing.
    fn stop_speaking(&self) {}

    /// Start a soft ambient thinking tone that loops until `stop_thinking_tone()`.
    /// Called when the LLM is inferring so the user hears that the system is working.
    /// Default implementation is a no-op (for PrintOutput / tests).
    fn start_thinking_tone(&self) {}

    /// Stop the thinking tone. Idempotent — safe to call when no tone is playing.
    fn stop_thinking_tone(&self) {}
}
