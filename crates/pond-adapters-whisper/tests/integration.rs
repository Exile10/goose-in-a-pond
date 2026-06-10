//! Integration tests for `WhisperInput` and `WhisperKeywordDetector`.
//!
//! `WhisperInput` is the legacy HTTP backend (whisper-server multipart POST).
//! It only compiles when `--features legacy-subprocess` is on, so this whole
//! file is gated. The detector tests construct a small mock backend that
//! implements `WhisperBackend`, exercising the no-feature path.

// ── Mock backend (default build) ──────────────────────────────────────────────

use anyhow::Result;
use pond_adapters_whisper::{WhisperBackend, WhisperKeywordDetector};
use pond_core::ports::wake_word::WakeWordDetector;
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

// ── Legacy HTTP backend tests ────────────────────────────────────────────────

#[cfg(feature = "legacy-subprocess")]
mod legacy {
    use pond_adapters_whisper::WhisperInput;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Load the jfk.wav fixture from the workspace-level tests/blobs/ directory.
    fn load_jfk_wav() -> Vec<u8> {
        let wav_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/blobs/jfk.wav");
        std::fs::read(&wav_path)
            .expect("tests/blobs/jfk.wav not found — run `cargo test` from workspace root")
    }

    #[tokio::test]
    async fn transcribe_wav_returns_text_from_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/inference"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "text": " And so my fellow Americans ask not what your country can do for you ask what you can do for your country"
            })))
            .mount(&server)
            .await;

        let whisper = WhisperInput::new(Some(&server.uri()));
        let result = whisper.transcribe_wav(load_jfk_wav()).await.unwrap();

        assert!(result.is_some(), "expected Some(text), got None");
        let text = result.unwrap();
        assert!(
            text.contains("ask not what your country can do for you"),
            "JFK quote missing from: {:?}",
            text
        );
    }

    #[tokio::test]
    async fn transcribe_wav_returns_none_for_empty_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/inference"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "text": "" })),
            )
            .mount(&server)
            .await;

        let whisper = WhisperInput::new(Some(&server.uri()));
        let result = whisper.transcribe_wav(load_jfk_wav()).await.unwrap();
        assert!(result.is_none(), "expected None for empty transcript");
    }

    #[tokio::test]
    async fn transcribe_wav_returns_none_for_whitespace_only_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/inference"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "text": "   " })),
            )
            .mount(&server)
            .await;

        let whisper = WhisperInput::new(Some(&server.uri()));
        let result = whisper.transcribe_wav(load_jfk_wav()).await.unwrap();
        assert!(
            result.is_none(),
            "expected None for whitespace-only transcript"
        );
    }

    #[tokio::test]
    async fn transcribe_wav_errors_on_server_500() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/inference"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&server)
            .await;

        let whisper = WhisperInput::new(Some(&server.uri()));
        let result = whisper.transcribe_wav(load_jfk_wav()).await;
        assert!(result.is_err(), "expected Err on HTTP 500");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("500"),
            "error message should mention status: {}",
            msg
        );
    }

    #[tokio::test]
    async fn transcribe_wav_errors_on_server_404() {
        let server = MockServer::start().await;
        // No mock mounted → wiremock returns 404 by default

        let whisper = WhisperInput::new(Some(&server.uri()));
        let result = whisper.transcribe_wav(load_jfk_wav()).await;
        assert!(result.is_err(), "expected Err on HTTP 404");
    }

    #[tokio::test]
    async fn transcribe_wav_posts_to_correct_path() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/inference"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "text": "ok" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let whisper = WhisperInput::new(Some(&server.uri()));
        whisper.transcribe_wav(load_jfk_wav()).await.unwrap();

        // wiremock asserts the `expect(1)` call count on Drop
    }

    /// Run with: `cargo test -p pond-adapters-whisper --features legacy-subprocess -- --ignored live_transcription`
    #[tokio::test]
    #[ignore = "requires local whisper.cpp server on port 9000"]
    async fn live_transcription_of_jfk_wav() {
        let whisper = WhisperInput::new(None);
        let result = whisper.transcribe_wav(load_jfk_wav()).await.unwrap();
        let text = result.expect("expected non-empty transcript for jfk.wav");
        println!("Transcript: {}", text);
        assert!(
            text.to_lowercase().contains("ask not what your country"),
            "JFK quote not found in live transcript: {:?}",
            text
        );
    }
}
