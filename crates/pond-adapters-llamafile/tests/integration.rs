//! Integration tests for LlamafileProvider.
//!
//! Mocks the llamafile HTTP server with wiremock — no real binary needed.
//!
//! Run: cargo test -p pond-adapters-llamafile

use pond_adapters_llamafile::{LlamafileProvider, DEFAULT_MODEL};
use pond_core::domain::message::{ChatMessage, Role};
use pond_core::ports::provider::LlmProvider;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn success_body(content: &str) -> serde_json::Value {
    serde_json::json!({
        "choices": [{"message": {"content": content}}]
    })
}

async fn complete_with_mock(response: ResponseTemplate) -> anyhow::Result<ChatMessage> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(response)
        .mount(&server)
        .await;
    let provider = LlamafileProvider::new(Some(&server.uri()));
    provider
        .complete("You are helpful.", vec![ChatMessage::user("Hello")])
        .await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn returns_assistant_message_on_success() {
    let reply =
        complete_with_mock(ResponseTemplate::new(200).set_body_json(success_body("Hi there!")))
            .await
            .unwrap();

    assert_eq!(reply.role, Role::Assistant);
    assert_eq!(reply.content, "Hi there!");
}

#[tokio::test]
async fn returns_error_on_non_200() {
    let err =
        complete_with_mock(ResponseTemplate::new(500).set_body_string("internal error"))
            .await
            .unwrap_err();

    assert!(err.to_string().contains("500"), "unexpected error: {err}");
}

#[tokio::test]
async fn returns_error_when_llamafile_offline() {
    // Port 1 is reserved — connection refused immediately.
    let provider = LlamafileProvider::new(Some("http://127.0.0.1:1"));
    let err = provider
        .complete("sys", vec![ChatMessage::user("hi")])
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("llamafile"),
        "expected 'llamafile' in error, got: {err}"
    );
}

#[tokio::test]
async fn model_name_is_llama_cpp() {
    let provider = LlamafileProvider::new(None);
    assert_eq!(provider.model_name(), DEFAULT_MODEL);
}

#[tokio::test]
async fn system_prompt_is_first_message_in_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body("ok")))
        .mount(&server)
        .await;

    let provider = LlamafileProvider::new(Some(&server.uri()));
    provider
        .complete("my system prompt", vec![ChatMessage::user("hi")])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "expected exactly one HTTP request");

    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().expect("messages must be array");

    // First message must be the system prompt.
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "my system prompt");

    // Second message must be the user input.
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "hi");
}
