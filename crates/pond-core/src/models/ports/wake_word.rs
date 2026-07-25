//! Driving Port: Wake-word detection
//!
//! `StreamingWakeWordDetector` is the primary interface. Implement it to plug in any
//! wake-word backend. The blanket impl provides `WakeWordDetector` automatically so
//! existing code that calls `wait_for_activation()` continues to compile unchanged.
//!
//! Implementations:
//! - `InstantActivation` — returns immediately (keyboard / stdin mode, tests)
//! - `WhisperKeywordDetector` — continuous ring-buffer + sliding window via whisper.cpp
//! - (future) `PorcupineDetector` — Picovoice Porcupine native KWS

use anyhow::Result;
use async_trait::async_trait;

// ── StreamingWakeWordDetector (primary interface) ─────────────────────────────

/// Audio captured during and after the wake word, ready to transcribe as a command.
///
/// When `captured_audio` is `Some`, the Listen state can transcribe this buffer
/// instead of starting a fresh microphone recording — enabling the "one-breath"
/// flow: "Hey Goose, what's the weather?" is handled in a single utterance.
pub struct WakeWordActivation {
    /// WAV-encoded bytes (16-bit mono 16 kHz) captured after the wake word,
    /// or `None` if the detector does not support audio hand-off.
    pub captured_audio: Option<Vec<u8>>,
}

/// **Primary wake-word port.**
///
/// Implement this trait to add a new wake-word detection backend. It returns
/// `WakeWordActivation` alongside the activation signal so callers can use
/// audio captured during the wake word utterance as the command audio — enabling
/// "Hey Goose, what's the weather?" to be handled in one breath.
///
/// A blanket impl of the deprecated `WakeWordDetector` is provided automatically,
/// so this trait works everywhere the old one was expected.
#[async_trait]
pub trait StreamingWakeWordDetector: Send + Sync {
    /// Block until the wake word is heard, then return any captured command audio.
    async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation>;

    /// Short label shown in the Wait state UI (e.g. `"Say \"Goose\"..."`).
    fn activation_prompt(&self) -> &str {
        "Waiting for activation..."
    }

    /// Whether this detector can meaningfully interrupt an in-flight turn.
    ///
    /// `run_loop` races the streaming agent turn against
    /// `wait_for_activation_with_audio()` so a spoken wake word can barge in
    /// mid-inference/mid-TTS. That race is only correct for detectors that
    /// actually **wait** for real audio.
    ///
    /// A detector that resolves *immediately* (e.g. [`InstantActivation`] in
    /// stdin / `--no-wake-word` / whisper-load-failure fallback) would win the
    /// race before any turn could complete, aborting every turn. Such detectors
    /// return `false` so `run_loop` awaits the turn directly without the
    /// interrupt race. Real streaming detectors keep the default `true`.
    ///
    /// [`InstantActivation`]: crate::models::services::instant_activation::InstantActivation
    fn supports_interruption(&self) -> bool {
        true
    }
}

// ── WakeWordDetector (deprecated, provided via blanket impl) ──────────────────

/// Simplified wake-word port: activates but does not capture audio.
///
/// **Deprecated** — implement `StreamingWakeWordDetector` instead.
/// This trait is retained only for blanket-impl compatibility and will be
/// removed in a future release.
#[async_trait]
#[deprecated(
    since = "0.2.0",
    note = "Implement `StreamingWakeWordDetector` instead. \
            `WakeWordDetector` is provided automatically via blanket impl."
)]
pub trait WakeWordDetector: Send + Sync {
    /// Block until the assistant should activate.
    async fn wait_for_activation(&self) -> Result<()>;

    /// Short label shown in the Wait state.
    fn activation_prompt(&self) -> &str {
        "Waiting for activation..."
    }
}

/// Blanket impl: every `StreamingWakeWordDetector` is also a `WakeWordDetector`.
#[async_trait]
#[allow(deprecated)]
impl<T: StreamingWakeWordDetector + 'static> WakeWordDetector for T {
    async fn wait_for_activation(&self) -> Result<()> {
        self.wait_for_activation_with_audio().await.map(|_| ())
    }

    fn activation_prompt(&self) -> &str {
        StreamingWakeWordDetector::activation_prompt(self)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::services::instant_activation::InstantActivation;

    #[test]
    fn wakeword_activation_no_audio_field_is_none() {
        let a = WakeWordActivation {
            captured_audio: None,
        };
        assert!(a.captured_audio.is_none());
    }

    #[test]
    fn wakeword_activation_with_audio_carries_bytes() {
        let wav = vec![b'R', b'I', b'F', b'F'];
        let a = WakeWordActivation {
            captured_audio: Some(wav.clone()),
        };
        assert_eq!(a.captured_audio.unwrap(), wav);
    }

    #[tokio::test]
    async fn instant_activation_satisfies_streaming_interface() {
        let det = InstantActivation;
        let activation = det.wait_for_activation_with_audio().await.unwrap();
        assert!(activation.captured_audio.is_none());
        assert!(!StreamingWakeWordDetector::activation_prompt(&det).is_empty());
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn instant_activation_satisfies_legacy_interface_via_blanket() {
        let det: &dyn WakeWordDetector = &InstantActivation;
        assert!(det.wait_for_activation().await.is_ok());
        // activation_prompt on the WakeWordDetector trait object is unambiguous here.
        assert!(!<dyn WakeWordDetector>::activation_prompt(det).is_empty());
    }
}
