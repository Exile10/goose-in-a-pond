//! Integration tests for `WhisperInput` and `WhisperKeywordDetector`.
//!
//! `WhisperInput` is the legacy HTTP backend (whisper-server multipart POST).
//! It only compiles when `--features legacy-subprocess` is on, so this whole
//! file is gated. The detector tests construct a small mock backend that
//! implements `WhisperBackend`, exercising the no-feature path.

// ── Mock backend (default build) ──────────────────────────────────────────────

use anyhow::Result;
use pond_adapters_whisper::{WhisperBackend, WhisperKeywordDetector};
use pond_core::models::ports::wake_word::WakeWordDetector;
use std::sync::Arc;

/// Minimal `WhisperBackend` for tests — always returns the canned transcript.
struct MockBackend(&'static str);
impl WhisperBackend for MockBackend {
    fn transcribe_pcm_blocking(&self, _samples: &[f32]) -> Result<String> {
        Ok(self.0.to_string())
    }
}

/// The activation prompt must mention the trigger word so the user knows
/// what to say.
#[test]
fn wake_word_detector_prompt_mentions_goose() {
    let detector = WhisperKeywordDetector::new(
        Arc::new(MockBackend("")) as Arc<dyn WhisperBackend>,
        "goose",
    );
    assert!(
        detector
            .activation_prompt()
            .to_lowercase()
            .contains("goose"),
        "expected 'goose' in prompt: {}",
        detector.activation_prompt()
    );
}

/// Verify the type satisfies the `WakeWordDetector` port so it can be
/// wired into `ChatService`.
#[test]
fn wake_word_detector_is_wake_word_detector_trait_object() {
    let _: Arc<dyn WakeWordDetector> = Arc::new(WhisperKeywordDetector::new(
        Arc::new(MockBackend("")) as Arc<dyn WhisperBackend>,
        "goose",
    ));
}
