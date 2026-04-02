//! Integration tests for OllamaProvider.
//!
//! These tests use a wiremock HTTP server in place of a real Ollama instance
//! and inspect the outgoing request body to verify that:
//!
//!   1. The system prompt is always the first message sent to Ollama.
//!   2. Multi-turn conversation history is forwarded in the correct order.
//!   3. `with_max_tokens()` serialises `options.num_predict` in the request.
//!   4. `with_temperature()` serialises `options.temperature` in the request.
//!   5. The `options` key is absent when neither is configured.
//!
//! The live smoke test at the bottom requires a running `ollama serve` and
//! a pulled model; it is ignored by default.

use pond_adapters_ollama::OllamaProvider;
use pond_core::domain::message::ChatMessage;
use pond_core::ports::provider::LlmProvider;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn ollama_ok(content: &str) -> serde_json::Value {
    serde_json::json!({
        "model": "llama3.2",
        "message": { "role": "assistant", "content": content },
        "done": true
    })
}

async fn mount_ok(server: &MockServer, content: &str) {
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ollama_ok(content)))
        .mount(server)
        .await;
}

// ── Request structure ─────────────────────────────────────────────────────────

/// The system prompt must appear as the very first message in the Ollama
/// request so the model receives its instructions before any user turn.
#[tokio::test]
async fn system_prompt_is_first_message_in_request() {
    let server = MockServer::start().await;
    mount_ok(&server, "ok").await;

    let provider = OllamaProvider::new(Some(&server.uri()), None);
    provider
        .complete("You are a helpful assistant.", vec![ChatMessage::user("Hi")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "expected exactly one request");

    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is not valid JSON");

    let first = &body["messages"][0];
    assert_eq!(first["role"], "system", "first message role must be 'system'");
    assert_eq!(
        first["content"], "You are a helpful assistant.",
        "system message content mismatch"
    );
}

/// All turns of a conversation must arrive at Ollama in chronological order
/// so the model has correct context.
#[tokio::test]
async fn multi_turn_history_sent_in_order() {
    let server = MockServer::start().await;
    mount_ok(&server, "response").await;

    let history = vec![
        ChatMessage::user("first"),
        ChatMessage::assistant("second"),
        ChatMessage::user("third"),
    ];

    let provider = OllamaProvider::new(Some(&server.uri()), None);
    provider.complete("sys", history).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is not valid JSON");

    // messages[0] is the system prompt; history follows from index 1.
    assert_eq!(body["messages"][1]["content"], "first");
    assert_eq!(body["messages"][2]["content"], "second");
    assert_eq!(body["messages"][3]["content"], "third");
}

// ── Options serialisation ─────────────────────────────────────────────────────

/// When `with_max_tokens()` is configured, `options.num_predict` must be
/// present in the request body.
#[tokio::test]
async fn max_tokens_sent_in_options_when_configured() {
    let server = MockServer::start().await;
    mount_ok(&server, "ok").await;

    let provider = OllamaProvider::new(Some(&server.uri()), None).with_max_tokens(128);
    provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is not valid JSON");

    assert_eq!(
        body["options"]["num_predict"], 128,
        "options.num_predict should be 128"
    );
}

/// When `with_temperature()` is configured, `options.temperature` must be
/// present in the request body.
#[tokio::test]
async fn temperature_sent_in_options_when_configured() {
    let server = MockServer::start().await;
    mount_ok(&server, "ok").await;

    let provider = OllamaProvider::new(Some(&server.uri()), None).with_temperature(0.3);
    provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is not valid JSON");

    let temp = body["options"]["temperature"]
        .as_f64()
        .expect("options.temperature should be a number");
    assert!(
        (temp - 0.3_f64).abs() < 1e-4,
        "expected temperature ≈ 0.3, got {}",
        temp
    );
}

/// When neither `with_max_tokens()` nor `with_temperature()` is called, the
/// `options` key must be absent so Ollama uses its own defaults.
#[tokio::test]
async fn options_absent_when_not_configured() {
    let server = MockServer::start().await;
    mount_ok(&server, "ok").await;

    let provider = OllamaProvider::new(Some(&server.uri()), None);
    provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body is not valid JSON");

    assert!(
        body.get("options").is_none() || body["options"].is_null(),
        "options should be absent when not configured; got: {}",
        body["options"]
    );
}

// ── Live smoke test ───────────────────────────────────────────────────────────

/// Run with:
///   `cargo test -p pond-adapters-ollama -- --ignored live_ollama`
///
/// Requires:
///   - `ollama serve` running on localhost:11434
///   - `ollama pull llama3.2` (or adjust the model below)
#[tokio::test]
#[ignore = "requires local Ollama instance with llama3.2 pulled"]
async fn live_ollama_completion() {
    let provider = OllamaProvider::new(None, Some("llama3.2")); // default: http://localhost:11434
    let reply = provider
        .complete(
            "You are a concise assistant. Reply in one sentence.",
            vec![ChatMessage::user("What is 2 + 2?")],
        )
        .await
        .expect("live Ollama completion failed");

    println!("Ollama replied: {}", reply.content);
    assert!(
        !reply.content.is_empty(),
        "expected non-empty response from Ollama"
    );
}
