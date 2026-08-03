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

    /// Nothing to prompt for: this detector never waits, so there is no
    /// moment to describe. It said "Type your message" for both callers, but
    /// `--no-wake-word` pairs it with a *microphone* — so a voice session
    /// announced a keyboard, twice per turn, next to the real "listening"
    /// prompt. `run_loop` skips an empty prompt.
    fn activation_prompt(&self) -> &str {
        ""
    }

    /// `InstantActivation` resolves immediately, so it must NOT participate in
    /// `run_loop`'s interrupt race — otherwise the wake future would win before
    /// any turn completes and every turn would be aborted. Returning `false`
    /// makes `run_loop` await the turn directly in stdin / `--no-wake-word` /
    /// whisper-load-failure fallback modes.
    fn supports_interruption(&self) -> bool {
        false
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

    #[test]
    fn instant_activation_does_not_support_interruption() {
        // Critical: InstantActivation resolves instantly, so run_loop must not
        // race turns against it. Guards the class of bug where every turn is
        // aborted before it can complete in stdin / --no-wake-word mode.
        let detector = InstantActivation;
        assert!(!detector.supports_interruption());
    }

    /// An empty prompt is the contract, not an oversight: this detector does
    /// not wait, so there is no waiting to describe. The previous text claimed
    /// the user should type, which is wrong whenever `--no-wake-word` is
    /// paired with a microphone. `run_loop` skips an empty prompt entirely.
    #[tokio::test]
    async fn instant_activation_announces_nothing_because_it_never_waits() {
        let detector = InstantActivation;
        // Explicitly call through StreamingWakeWordDetector to avoid ambiguity
        // with the deprecated WakeWordDetector blanket impl.
        assert_eq!(
            StreamingWakeWordDetector::activation_prompt(&detector),
            "",
            "a detector that returns instantly must not prompt for anything"
        );
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
