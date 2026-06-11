//! InstantActivation — StreamingWakeWordDetector for keyboard / stdin mode.
//!
//! Returns immediately from `wait_for_activation_with_audio()` so the workflow loop
//! transitions straight from Wait to Listen without pausing.
//! Used as the default in `ChatService` and in all tests.
//!
//! Implements `StreamingWakeWordDetector`; the blanket impl provides `WakeWordDetector`
//! automatically.

use crate::models::ports::wake_word::{StreamingWakeWordDetector, WakeWordActivation};
use anyhow::Result;
use async_trait::async_trait;

/// No-op wake-word detector: activates instantly without waiting.
///
/// Use this when the user drives the loop via keyboard (stdin) or when
/// tests need deterministic, non-blocking behaviour.
pub struct InstantActivation;

#[async_trait]
impl StreamingWakeWordDetector for InstantActivation {
    async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
        Ok(WakeWordActivation {
            captured_audio: None,
        })
    }

    fn activation_prompt(&self) -> &str {
        "Type your message"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ports::wake_word::WakeWordDetector;

    #[tokio::test]
    async fn instant_activation_returns_ok_immediately() {
        let detector = InstantActivation;
        let activation = detector.wait_for_activation_with_audio().await.unwrap();
        assert!(activation.captured_audio.is_none());
    }

    #[tokio::test]
    async fn instant_activation_prompt_is_non_empty() {
        let detector = InstantActivation;
        // Explicitly call through StreamingWakeWordDetector to avoid ambiguity
        // with the deprecated WakeWordDetector blanket impl.
        assert!(!StreamingWakeWordDetector::activation_prompt(&detector).is_empty());
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn instant_activation_satisfies_wake_word_detector_via_blanket() {
        // The blanket impl provides WakeWordDetector automatically.
        let detector = InstantActivation;
        let det: &dyn WakeWordDetector = &detector;
        assert!(det.wait_for_activation().await.is_ok());
    }
}
