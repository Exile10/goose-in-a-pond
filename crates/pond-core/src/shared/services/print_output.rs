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
}
