//! Provider routing integration tests for POST /api/v1/chat/stream.
//!
//! Verifies that the ModelRouter correctly routes chat/think/task messages
//! to the appropriate LLM provider, error events are emitted when providers fail,
//! and usage stats are included in the done event when providers report them.
//!
//! All tests use a wiremock server as the llamafile backend — no live services needed.
//!
//! Run: cargo test -p pond-api --test provider_routing_test

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_adapters_llamafile::LlamafileProvider;
use pond_api::{build_router, AppState};
use pond_core::domain::onboarding::OnboardingStep;
use pond_core::ports::agent::{Agent, AgentRequest, AgentResponse};
use pond_core::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::ports::extension_manager::{AddExtensionRequest, ExtensionInfo, ExtensionManagerPort};
use pond_core::ports::onboarding::OnboardingRepository;
use pond_core::services::mock_agent::MockAgent;
use pond_core::services::mock_memory::MockMemoryRepository;
use pond_core::services::mock_profile::MockProfileRepository;
use pond_core::services::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::services::mock_settings::MockSettingsRepository;
use pond_core::services::model_router::ModelRouter;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use reqwest::Client as ReqwestClient;
use std::sync::Arc;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Stubs ─────────────────────────────────────────────────────────────────────

struct CompletedOnboarding;

#[async_trait::async_trait]
impl OnboardingRepository for CompletedOnboarding {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        Some(OnboardingStep::Completed)
    }
    async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> { Ok(()) }
    async fn reset(&self) -> anyhow::Result<()> { Ok(()) }
}

struct NoDevices;

#[async_trait::async_trait]
impl DeviceRegistry for NoDevices {
    async fn register(&self, req: RegisterDeviceRequest) -> anyhow::Result<Device> {
        Ok(Device {
            id: "mock".to_string(),
            name: req.name,
            device_type: req.device_type,
            hostname: req.hostname,
            ip_address: None,
            capabilities: req.capabilities,
            registered_at: "2024-01-01 00:00:00".to_string(),
            last_seen: None,
            is_online: false,
        })
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> { Ok(vec![]) }
    async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> { Ok(None) }
    async fn unregister(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
    async fn heartbeat(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
}

struct StubExtensionManager;

#[async_trait::async_trait]
impl ExtensionManagerPort for StubExtensionManager {
    async fn list_extensions(&self) -> anyhow::Result<Vec<ExtensionInfo>> {
        Ok(vec![ExtensionInfo {
            name: "giap".to_string(),
            kind: "builtin".to_string(),
            description: "GIAP builtin tools".to_string(),
            tools: vec!["giap__get_current_weather".to_string()],
        }])
    }

    async fn add_extension(&self, _request: AddExtensionRequest) -> anyhow::Result<ExtensionInfo> {
        anyhow::bail!("not implemented in test")
    }

    async fn remove_extension(&self, _name: &str) -> anyhow::Result<()> {
        anyhow::bail!("not implemented in test")
    }

    async fn list_tools(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec!["giap__get_current_weather".to_string()])
    }
}

struct ToolCallingAgent;

#[async_trait::async_trait]
impl Agent for ToolCallingAgent {
    async fn chat(&self, _request: AgentRequest) -> anyhow::Result<AgentResponse> {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "tool_calls".to_string(),
            serde_json::json!(["giap__get_current_weather"]).to_string(),
        );
        Ok(AgentResponse {
            text: "Task completed from agent".to_string(),
            metadata,
        })
    }
}

// ── SSE helpers ───────────────────────────────────────────────────────────────

/// Build a minimal OpenAI-compatible SSE body with optional usage in the final chunk.
fn sse_body_with_usage(tokens: &[&str], usage: Option<(u32, u32)>) -> String {
    let mut body = String::new();
    for tok in tokens {
        let escaped = tok.replace('"', "\\\"");
        body.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{escaped}\"}},\"finish_reason\":null}}]}}\n\n"
        ));
    }
    // Final chunk — includes usage if requested
    if let Some((prompt, completion)) = usage {
        body.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"\"}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":{prompt},\"completion_tokens\":{completion},\"total_tokens\":{}}}}}\n\n",
            prompt + completion
        ));
    } else {
        body.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"\"},\"finish_reason\":\"stop\"}]}\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

/// Build an AppState with the given provider wired into llm_provider.
async fn make_app_with_provider(
    provider: Arc<dyn pond_core::ports::provider::LlmProvider>,
) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(Some(provider))),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        download_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
    });
    (build_router(state, std::path::PathBuf::from("web/dist")), tmp)
}

fn stream_request(body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/chat/stream")
        .header("content-type", "application/json")
        .header("Authorization", "Bearer test-token")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn collect_sse_events(body: axum::body::Body) -> Vec<serde_json::Value> {
    use axum::body::to_bytes;
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .collect()
}

fn done_event(events: &[serde_json::Value]) -> Option<&serde_json::Value> {
    events.iter().find(|e| e.get("done").and_then(|d| d.as_bool()).unwrap_or(false))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A message classified as "chat" should have model_role = "chat" in the done event.
#[tokio::test]
async fn chat_message_routes_to_chat_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body_with_usage(&["Hello!"], None))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = Arc::new(ModelRouter::new(llamafile.clone(), llamafile.clone(), llamafile));
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "hello there"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = done_event(&events).expect("no done event");
    assert_eq!(
        done["model_role"].as_str(),
        Some("chat"),
        "expected model_role=chat, got: {:?}",
        done
    );
}

/// A reasoning-type message should route to the think provider.
#[tokio::test]
async fn think_message_routes_to_think_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body_with_usage(&["Because light scatters."], None))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = Arc::new(ModelRouter::new(llamafile.clone(), llamafile.clone(), llamafile));
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "explain why the sky is blue"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = done_event(&events).expect("no done event");
    assert_eq!(done["model_role"].as_str(), Some("think"));
}

/// An action/reminder message should route to the task provider.
#[tokio::test]
async fn task_message_routes_to_task_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body_with_usage(&["Reminder set."], None))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = Arc::new(ModelRouter::new(llamafile.clone(), llamafile.clone(), llamafile));
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "remind me to call mum at 9am"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = done_event(&events).expect("no done event");
    assert_eq!(done["model_role"].as_str(), Some("task"));
}

/// When the provider returns a non-200, the SSE stream should include an error event.
#[tokio::test]
async fn error_event_emitted_when_provider_returns_500() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = Arc::new(ModelRouter::new(llamafile.clone(), llamafile.clone(), llamafile));
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "hello"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let error_event = events.iter().find(|e| e.get("error").is_some());
    assert!(
        error_event.is_some(),
        "expected an error event when provider returns 503, got events: {:?}",
        events
    );
}

/// The done event must always include a non-empty session_id.
#[tokio::test]
async fn done_event_has_session_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body_with_usage(&["Hi"], None))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = Arc::new(ModelRouter::new(llamafile.clone(), llamafile.clone(), llamafile));
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "hi"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = done_event(&events).expect("no done event");

    let session_id = done["session_id"].as_str().unwrap_or("");
    assert!(!session_id.is_empty(), "done event has empty session_id: {:?}", done);
}

/// When the provider SSE response includes a `usage` field, the done event must
/// include `usage.prompt_tokens` and `usage.completion_tokens`.
#[tokio::test]
async fn done_event_has_usage_when_provider_includes_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_body_with_usage(&["Hello!"], Some((12, 47))))
                .append_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let llamafile = Arc::new(LlamafileProvider::new(Some(&server.uri())));
    let router = Arc::new(ModelRouter::new(llamafile.clone(), llamafile.clone(), llamafile));
    let (app, _tmp) = make_app_with_provider(router).await;

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "hello"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let done = done_event(&events).expect("no done event");

    let prompt = done["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let completion = done["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    assert_eq!(prompt, 12, "expected prompt_tokens=12 in done event: {:?}", done);
    assert_eq!(completion, 47, "expected completion_tokens=47 in done event: {:?}", done);
}

/// When no provider is configured and a non-task message is sent, the stream
/// emits an explicit error event.
#[tokio::test]
async fn no_provider_emits_error_event_for_non_task_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        download_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
    });
    let app = build_router(state, std::path::PathBuf::from("web/dist"));

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "echo test"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let error_event = events.iter().find(|e| e.get("error").is_some());
    assert!(error_event.is_some(), "expected error event: {:?}", events);
    assert_eq!(
        error_event
            .and_then(|e| e.get("error"))
            .and_then(|e| e.as_str()),
        Some("no LLM provider configured")
    );
}

/// When task routing is active and Goose extensions are available, task
/// requests run through the agent loop even if llm_provider is None.
#[tokio::test]
async fn task_message_uses_agent_with_tool_call_events_without_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(ToolCallingAgent),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: Some(Arc::new(StubExtensionManager)),
        mcp_server_repo: None,
        download_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
    });
    let app = build_router(state, std::path::PathBuf::from("web/dist"));

    let resp = app
        .oneshot(stream_request(serde_json::json!({"message": "remind me to water plants at 6"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let events = collect_sse_events(resp.into_body()).await;
    let tool_event = events.iter().find(|e| {
        e.get("type").and_then(|t| t.as_str()) == Some("tool_call")
    });
    assert!(tool_event.is_some(), "expected tool_call event: {:?}", events);

    let done = done_event(&events).expect("no done event");
    assert_eq!(done["model_role"].as_str(), Some("task"));
}
