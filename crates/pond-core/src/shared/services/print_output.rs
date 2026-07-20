//! PrintOutput — default VoiceOutput that writes to stdout.
//!
//! Used when no TTS engine is configured (stdin/keyboard mode, tests).
//! Moves the bare `println!` from the Speak state into the port so
//! `ChatService` never has a hard dependency on stdout.

use crate::models::ports::voice_output::VoiceOutput;
use anyhow::Result;
use async_trait::async_trait;

/// VoiceOutput that prints the text to stdout instead of speaking it.
pub struct PrintOutput;

impl Default for PrintOutput {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl VoiceOutput for PrintOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        println!("  🗣  {}", text);
        Ok(())
    }
}

/// VoiceOutput that discards all output. Used when the response is surfaced by
/// another channel and stdout must stay clean — e.g. `pond-server chat
/// --json-events`, where the assistant text is streamed as NDJSON `token`
/// events and stdout carries NOTHING but the JSON contract lines. Also handy
/// for headless runs and tests that assert on the event sink, not on TTS.
pub struct SilentOutput;

impl Default for SilentOutput {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl VoiceOutput for SilentOutput {
    async fn speak(&self, _text: &str) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn print_output_speak_returns_ok() {
        let out = PrintOutput;
        assert!(out.speak("hello world").await.is_ok());
    }

    #[tokio::test]
    async fn print_output_compiles_as_voice_output() {
        // Verify it type-checks as Arc<dyn VoiceOutput>
        let _out: Arc<dyn VoiceOutput> = Arc::new(PrintOutput);
    }

    #[tokio::test]
    async fn silent_output_speaks_nothing() {
        let out = SilentOutput;
        // speak() must succeed and write nothing to stdout.
        assert!(out.speak("this must not print").await.is_ok());
        let _out: Arc<dyn VoiceOutput> = Arc::new(SilentOutput);
    }
}
