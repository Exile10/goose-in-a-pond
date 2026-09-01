//! Full pipeline integration tests — Wait → Listen → Think → Speak.
//!
//! These tests wire real adapter types together with mocked HTTP back-ends
//! (wiremock) to exercise the entire workflow loop end-to-end without
//! requiring physical hardware (microphone, speakers, GPU).
//!
//! # Scenarios covered
//!
//! | Test                                      | Input         | LLM    | Output         |
//! |-------------------------------------------|---------------|--------|----------------|
//! | `text_mode_listen_think_speak`            | scripted text | Ollama | captured text  |
//! | `multi_turn_history_preserved`            | 2-turn script | Ollama | captured text  |
//! | `voice_mode_whisper_ollama_pipeline`      | WAV → Whisper | Ollama | captured text  |
//! | `live_full_voice_loop` (ignored)          | mic           | Ollama | piper audio    |
//!
//! The `#[ignore]`d live test requires a real microphone, whisper.cpp server
//! on port 9000, Ollama on port 11434, and the piper binary + model.

use anyhow::Result;
use async_trait::async_trait;
use pond_adapters_ollama::OllamaProvider;
use pond_core::models::domain::message::Role;
use pond_core::models::ports::voice_input::VoiceInput;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::shared::services::chat::ChatService;
use pond_core::user_data::mocks::mock_session::InMemorySessionStorage;
use pond_core::user_data::ports::session_storage::SessionStorage;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Test doubles ──────────────────────────────────────────────────────────────

/// Plays back a pre-loaded script of messages, returning `None` when the
/// script is exhausted (signals end-of-input to `ChatService::run_loop`).
struct ScriptedVoiceInput {
    script: Mutex<VecDeque<Option<String>>>,
}

impl ScriptedVoiceInput {
    fn new(lines: impl IntoIterator<Item = &'static str>) -> Self {
        let mut deque: VecDeque<Option<String>> =
            lines.into_iter().map(|s| Some(s.to_string())).collect();
        deque.push_back(None); // trailing None → end-of-input
        Self {
            script: Mutex::new(deque),
        }
    }
}

#[async_trait]
impl VoiceInput for ScriptedVoiceInput {
    async fn listen(&self) -> Result<Option<String>> {
        Ok(self.script.lock().unwrap().pop_front().flatten())
    }
}

/// Collects every string passed to `speak()` so tests can assert on output.
#[derive(Default)]
struct CapturingVoiceOutput {
    spoken: Mutex<Vec<String>>,
}

impl CapturingVoiceOutput {
    fn spoken(&self) -> Vec<String> {
        self.spoken.lock().unwrap().clone()
    }
}

#[async_trait]
impl VoiceOutput for CapturingVoiceOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        self.spoken.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn ollama_response(content: &str) -> serde_json::Value {
    serde_json::json!({
        "model": "llama3.2",
        "message": { "role": "assistant", "content": content },
        "done": true
    })
}

// Only the `legacy-subprocess`-gated voice pipeline test builds a service via
// this helper; gate it too so the default build has no dead-code warning.

// ── Text mode pipeline ────────────────────────────────────────────────────────

/// Full text-mode loop exercising all four states:
///   Wait (InstantActivation) → Listen (ScriptedVoiceInput) →
///   Think (Agent port = MockAgent echo) → Speak (CapturingVoiceOutput)
///
/// Uses `run_loop()` so the Speak state is actually reached — `chat_once()`
/// alone does not invoke the voice output component.
///
/// Regression guard for the InstantActivation race: with the default
/// InstantActivation detector, `run_loop` used to abort every turn via the
/// interrupt race before it could complete, so voice output stayed empty. The
/// turn must now complete and reach `speak()`.
///
/// Note: the Speak state consumes the `Agent` port's response, NOT the
/// `LlmProvider`. In tests the agent is `MockAgent`, which echoes the input;
/// the provider is only used for session-title generation. So the spoken text
/// is the echoed utterance, not the wiremock Ollama body.
#[tokio::test]
async fn text_mode_listen_think_speak() {
    let agent = Arc::new(MockAgent::new());
    let storage = Arc::new(InMemorySessionStorage::new());
    let session_id = "text-mode-test".to_string();
    storage.create_session(session_id.clone()).await.unwrap();
    let output = Arc::new(CapturingVoiceOutput::default());
    let input = Arc::new(ScriptedVoiceInput::new(["What is the capital of France?"]));

    // InstantActivation is the default wake-word detector in ChatService::new()
    let svc = ChatService::new(agent, session_id, storage)
        .with_voice_input(input)
        .with_voice_output(output.clone());

    svc.run_loop().await.unwrap();

    // The turn completed and the agent's (echoed) response reached the speaker.
    assert!(
        output
            .spoken()
            .iter()
            .any(|s| s.contains("What is the capital of France?")),
        "voice output should have received the agent response; got: {:?}",
        output.spoken()
    );
}

/// Multi-turn text pipeline: two `chat_once()` calls on the same session must
/// both persist to the authoritative session store, in order.
///
/// Architecture note: `ChatService` routes chat turns through the `Agent` port
/// (GooseAdapter in prod, `MockAgent` echo in tests), which owns its own
/// conversation history. The `LlmProvider` (wiremock Ollama here) is used only
/// for session-title generation, not for chat completion — so history is NOT
/// sent to Ollama on chat turns. This test therefore asserts persistence into
/// `SessionStorage` (the single source of truth the REST API reads), which is
/// where multi-turn continuity actually lives.
#[tokio::test]
async fn multi_turn_history_preserved_across_chat_once_calls() {
    let server = MockServer::start().await;
    // The provider is only hit for title generation; respond to any request.
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ollama_response("A short title.")))
        .mount(&server)
        .await;

    let agent = Arc::new(MockAgent::new());
    let storage = Arc::new(InMemorySessionStorage::new());
    let session_id = "multi-turn-test".to_string();
    storage.create_session(session_id.clone()).await.unwrap();
    let provider = Arc::new(OllamaProvider::new(Some(&server.uri()), Some("llama3.2")));

    let svc = ChatService::new(agent, session_id.clone(), storage.clone())
        .with_provider(provider)
        .with_voice_output(Arc::new(CapturingVoiceOutput::default()));

    let first = svc
        .chat_once("What is your name?".to_string())
        .await
        .unwrap();
    let second = svc
        .chat_once("What did you say your name was?".to_string())
        .await
        .unwrap();

    // The Agent port echoes the input.
    assert_eq!(first, "Echo: What is your name?");
    assert_eq!(second, "Echo: What did you say your name was?");

    // Both turns are persisted, in order, to the authoritative session store.
    let messages = storage.get_messages(&session_id).await.unwrap();
    let contents: Vec<&str> = messages
        .iter()
        .map(|m| m.message.content.as_str())
        .collect();
    assert_eq!(
        contents,
        vec![
            "What is your name?",
            "Echo: What is your name?",
            "What did you say your name was?",
            "Echo: What did you say your name was?",
        ],
        "both turns must persist to session storage in order; got: {contents:?}"
    );
}

// ── Voice mode pipeline (whisper → ollama) ────────────────────────────────────

/// Partial voice pipeline test: WAV bytes → WhisperInput (wiremock) →
/// OllamaProvider (wiremock) → CapturingVoiceOutput.
///
/// This covers the Listen (Whisper) and Think (Ollama) steps without needing
/// a real microphone — the test feeds a pre-recorded WAV file directly to
/// `WhisperInput::transcribe_wav()`, then passes the transcript to
/// `ChatService::chat_once()`.
///
/// The `WhisperInput` half of that description is historical: the HTTP backend
/// was deleted in 2026-08 and the test below no longer builds one. What remains
/// is the Think half — Ollama through `ChatService` — plus the role assignment.
/// The in-process recogniser has its own coverage in `pond-adapters-whisper`.

/// Verify correct role assignment across all pipeline steps:
/// user messages must be Role::User and LLM responses Role::Assistant.
#[tokio::test]
async fn pipeline_message_roles_are_correct() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ollama_response("42")))
        .mount(&server)
        .await;

    let agent = Arc::new(MockAgent::new());
    let storage = Arc::new(InMemorySessionStorage::new());
    let session_id = "roles-test-session".to_string();
    storage.create_session(session_id.clone()).await.unwrap();

    let svc = ChatService::new(agent, session_id.clone(), storage.clone())
        .with_provider(Arc::new(OllamaProvider::new(
            Some(&server.uri()),
            Some("llama3.2"),
        )))
        .with_voice_output(Arc::new(CapturingVoiceOutput::default()));

    svc.chat_once("What is 6 times 7?".to_string())
        .await
        .unwrap();

    let messages = storage.get_messages(&session_id).await.unwrap();
    assert!(messages.len() >= 2, "expected at least 2 messages");
    assert_eq!(
        messages[0].message.role,
        Role::User,
        "first message must be User"
    );
    assert_eq!(
        messages[1].message.role,
        Role::Assistant,
        "second message must be Assistant"
    );
}

// ── Fixture ───────────────────────────────────────────────────────────────────
