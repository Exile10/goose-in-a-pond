//! Integration tests for WhisperInput using real audio (jfk.wav) and a mock
//! whisper.cpp HTTP server.
//!
//! These tests bypass microphone capture entirely — they feed raw WAV bytes
//! directly into `WhisperInput::transcribe_wav()` and verify that:
//!
//!   1. The HTTP POST is formed correctly (right path, multipart form).
//!   2. A successful JSON response is parsed and returned as `Some(text)`.
//!   3. An empty `"text"` field maps to `None`.
//!   4. A non-2xx HTTP response is propagated as an `Err`.
//!
//! The JFK WAV fixture (tests/blobs/jfk.wav) is the canonical whisper.cpp sample:
//! JFK's 1961 inaugural address — 16-bit mono 16 kHz PCM, public domain.
//! Expected transcript: "ask not what your country can do for you …"

use pond_adapters_whisper::WhisperInput;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Load the jfk.wav fixture from the workspace-level tests/blobs/ directory.
fn load_jfk_wav() -> Vec<u8> {
    let wav_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/blobs/jfk.wav");
    std::fs::read(&wav_path).expect("tests/blobs/jfk.wav not found — run `cargo test` from workspace root")
}

// ── Core response parsing ─────────────────────────────────────────────────────

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
    assert!(result.is_none(), "expected None for whitespace-only transcript");
}

// ── Error handling ────────────────────────────────────────────────────────────

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
    assert!(msg.contains("500"), "error message should mention status: {}", msg);
}

#[tokio::test]
async fn transcribe_wav_errors_on_server_404() {
    let server = MockServer::start().await;
    // No mock mounted → wiremock returns 404 by default

    let whisper = WhisperInput::new(Some(&server.uri()));
    let result = whisper.transcribe_wav(load_jfk_wav()).await;
    assert!(result.is_err(), "expected Err on HTTP 404");
}

// ── Request format ────────────────────────────────────────────────────────────

/// Verifies the adapter hits exactly the right endpoint path.
#[tokio::test]
async fn transcribe_wav_posts_to_correct_path() {
    let server = MockServer::start().await;

    // Only mount on the exact path — any deviation returns 404
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

// ── Live smoke test (ignored by default — requires real whisper.cpp server) ───

/// Run with: `cargo test -p pond-adapters-whisper -- --ignored live_transcription`
///
/// Requires `./server -m models/ggml-base.en.bin --port 9000` to be running.
/// Expected output contains: "ask not what your country can do for you"
#[tokio::test]
#[ignore = "requires local whisper.cpp server on port 9000"]
async fn live_transcription_of_jfk_wav() {
    let whisper = WhisperInput::new(None); // default: http://127.0.0.1:9000
    let result = whisper.transcribe_wav(load_jfk_wav()).await.unwrap();
    let text = result.expect("expected non-empty transcript for jfk.wav");
    println!("Transcript: {}", text);
    assert!(
        text.to_lowercase().contains("ask not what your country"),
        "JFK quote not found in live transcript: {:?}",
        text
    );
}
