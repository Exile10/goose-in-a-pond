//! `FallbackVoiceOutput` — try the primary TTS engine; if it errors, use the fallback.
//!
//! Mirrors `FallbackProvider` for LLM — the pattern is identical:
//! attempt primary, log a warning on failure, retry with fallback.
//!
//! Typical wiring:
//! ```ignore
//! let tts = Arc::new(FallbackVoiceOutput::new(
//!     Arc::new(PiperOutput::new(primary_bin, primary_model)),
//!     Arc::new(PiperOutput::new(bin, model)), // fallback
//! ));
//! ```

use crate::ports::voice_output::VoiceOutput;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

pub struct FallbackVoiceOutput {
    primary: Arc<dyn VoiceOutput>,
    fallback: Arc<dyn VoiceOutput>,
}

impl FallbackVoiceOutput {
    pub fn new(primary: Arc<dyn VoiceOutput>, fallback: Arc<dyn VoiceOutput>) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl VoiceOutput for FallbackVoiceOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        match self.primary.speak(text).await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::info!("Primary TTS unavailable ({}), using fallback.", e);
                self.fallback.speak(text).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct SucceedingTts(Arc<AtomicBool>);
    struct FailingTts;

    #[async_trait]
    impl VoiceOutput for SucceedingTts {
        async fn speak(&self, _: &str) -> Result<()> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait]
    impl VoiceOutput for FailingTts {
        async fn speak(&self, _: &str) -> Result<()> {
            Err(anyhow::anyhow!("TTS server offline"))
        }
    }

    #[tokio::test]
    async fn uses_primary_when_it_succeeds() {
        let primary_called = Arc::new(AtomicBool::new(false));
        let fallback_called = Arc::new(AtomicBool::new(false));

        let tts = FallbackVoiceOutput::new(
            Arc::new(SucceedingTts(primary_called.clone())),
            Arc::new(SucceedingTts(fallback_called.clone())),
        );

        tts.speak("hello").await.unwrap();
        assert!(
            primary_called.load(Ordering::SeqCst),
            "primary should have been called"
        );
        assert!(
            !fallback_called.load(Ordering::SeqCst),
            "fallback should NOT have been called"
        );
    }

    #[tokio::test]
    async fn falls_back_when_primary_fails() {
        let fallback_called = Arc::new(AtomicBool::new(false));

        let tts = FallbackVoiceOutput::new(
            Arc::new(FailingTts),
            Arc::new(SucceedingTts(fallback_called.clone())),
        );

        tts.speak("hello").await.unwrap();
        assert!(
            fallback_called.load(Ordering::SeqCst),
            "fallback should have been called"
        );
    }

    #[tokio::test]
    async fn returns_error_when_both_fail() {
        let tts = FallbackVoiceOutput::new(Arc::new(FailingTts), Arc::new(FailingTts));
        let err = tts.speak("hello").await.unwrap_err();
        assert!(err.to_string().contains("TTS server offline"));
    }
}
