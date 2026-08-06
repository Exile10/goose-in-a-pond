//! PAI-4 P7 — `POST /api/v1/sessions/:id/compact`, the manual compaction axis.
//!
//! What these tests are for, in order of how much they matter:
//!
//! 1. **The button does not bypass P6's rate limiter.** This is the whole risk of
//!    the phase. `should_compact` is monotone above 75%, so an endpoint that ran
//!    a pass on every press would let anything holding a bearer token queue
//!    unbounded summarisation model calls in front of the user's next turn on a
//!    serial on-device engine. `a_second_press_is_refused_by_the_cooldown` is the
//!    guard: break the `claim_compaction` call in `compact_session` and it fails.
//! 2. **The endpoint reports rather than no-ops.** Every refusal carries a reason
//!    and the session's real utilisation, so a control that declines is
//!    diagnosable instead of looking broken.
//! 3. **A session id that does not exist is a 404**, not a healthy window.
//!
//! These are wiring tests. The cooldown arithmetic itself is unit-tested in
//! `pond-core`'s `context_monitor`; what no unit test can reach is whether the
//! route actually goes through it.
//!
//! Run: cargo test -p pond-api --test manual_compaction_test

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::mocks::mock_provider::MockProvider;
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::session::SessionMessage;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use reqwest::Client as ReqwestClient;
use serde_json::Value;
use tower::ServiceExt;

// ── Minimal stubs ──────────────────────────────────────────────────────────────

struct CompletedOnboarding;

#[async_trait::async_trait]
impl OnboardingRepository for CompletedOnboarding {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        Some(OnboardingStep::Completed)
    }
    async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> {
        Ok(())
    }
    async fn reset(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct MockDeviceRegistry;

#[async_trait::async_trait]
impl DeviceRegistry for MockDeviceRegistry {
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
            room: req.room,
        })
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        Ok(vec![])
    }
    async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
        Ok(None)
    }
    async fn unregister(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

// ── Test fixture ───────────────────────────────────────────────────────────────

async fn make_app() -> (axum::Router, Arc<AppState>, tempfile::TempDir) {
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
        transcribe_audio: None,
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(MockAgent::new()),
        // The summariser the pass runs on. Without one the endpoint answers
        // `no_summariser` and every assertion below would be about that branch.
        llm_provider: Arc::new(tokio::sync::RwLock::new(Some(
            Arc::new(MockProvider::new()),
        ))),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
        commissioner: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        tool_registry: None,
        marketplace: None,
        secret_repo: None,
        download_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
        operational_log: None,
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        memory_extractor: None,
        memory_extraction_service: None,
        last_user_activity: Arc::new(tokio::sync::RwLock::new(std::time::Instant::now())),
        consolidation_cancel: Arc::new(tokio::sync::RwLock::new(None)),
        consolidation_event_tx: tokio::sync::broadcast::channel(16).0,
        consolidation_runner: None,
        inference_pool: None,
        schedule_result_tx: tokio::sync::broadcast::channel(1).0,
        telemetry: None,
        context_monitor: Arc::new(
            pond_core::models::services::context_monitor::ContextMonitor::new(),
        ),
        mcp_app_resources: std::collections::HashMap::new(),
        oauth_state: pond_api::oauth_callback::new_oauth_state(),
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
        weather_provider: None,
    });
    (
        build_router(state.clone(), std::path::PathBuf::from("pond-desktop/dist")),
        state,
        tmp,
    )
}

fn compact_request(session_id: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/api/v1/sessions/{session_id}/compact"))
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap()
}

/// Status code first, then the body — a body predicate read off an error payload
/// reports the opposite of the truth.
async fn compact(app: &axum::Router, session_id: &str) -> Value {
    let resp = app
        .clone()
        .oneshot(compact_request(session_id))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "POST /sessions/{session_id}/compact did not answer 200",
    );
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).expect("compact response is not JSON")
}

/// Seed enough history that the rolling-summary refresh has something to fold:
/// it keeps the newest 6 messages verbatim and needs at least 4 older ones.
async fn seed_history(state: &Arc<AppState>, session_id: &str, count: usize) {
    for i in 0..count {
        let msg = if i % 2 == 0 {
            ChatMessage::user(format!("user message {i} about the greenhouse fans"))
        } else {
            ChatMessage::assistant(format!("assistant reply {i} about the greenhouse fans"))
        };
        state
            .session_storage
            .add_message(
                session_id.to_string(),
                SessionMessage::new(format!("m{i}"), session_id.to_string(), msg),
            )
            .await
            .expect("seed message");
    }
}

/// Put the session where the pressure axis would already be firing.
fn saturate(state: &Arc<AppState>, session_id: &str) {
    state.context_monitor.record_turn(session_id, 7000, 8192);
    let health = state.context_monitor.check_context_health(session_id);
    assert!(
        health.should_compact,
        "fixture is not under pressure ({}%) - every assertion about the \
         compaction path would be an assertion about the not_under_pressure \
         branch instead",
        health.utilization_pct,
    );
}

// ── Tests ──────────────────────────────────────────────────────────────────────

/// THE GUARD. Two presses in a row must not buy two model calls.
///
/// `claim_compaction` grants at most one pass per `COMPACTION_COOLDOWN_TURNS`
/// recorded turns, and the manual endpoint is deliberately *inside* that rule
/// rather than beside it. Nothing records a turn between these two requests, so
/// the cooldown cannot have expired and the second press must be refused.
#[tokio::test]
async fn a_second_press_is_refused_by_the_cooldown() {
    let (app, state, _tmp) = make_app().await;
    let session = state
        .session_storage
        .create_session("p7-cooldown".to_string())
        .await
        .expect("create session");
    seed_history(&state, &session.id, 12).await;
    saturate(&state, &session.id);

    let first = compact(&app, &session.id).await;
    assert_eq!(
        first["status"], "compacted",
        "the first press did not run a pass: {first}",
    );
    assert_eq!(first["outcome"], "refreshed");

    let second = compact(&app, &session.id).await;
    // `outcome` is null exactly when no pass ran, and that is the claim that
    // matters. `status` alone is too weak to be the guard: a second pass finds
    // the through-pointer already advanced and answers `NothingToDo`, which is
    // also reported as "skipped" — so a bypassed rate limiter would still look
    // like a refusal here while having spent the model call.
    assert!(
        second["outcome"].is_null(),
        "the second press ran a second pass - the manual endpoint is bypassing \
         P6's rate limiter, and a client hammering it would stack summarisation \
         model calls in front of the user's next turn on a serial on-device \
         engine: {second}",
    );
    assert_eq!(
        second["reason"], "cooling_down",
        "the second press was refused, but not by the cooldown: {second}",
    );
}

/// A refusal has to say why. A control that silently does nothing is
/// indistinguishable from a broken one, and this is the refusal a user will hit
/// most: pressing the button on a conversation that is nowhere near full.
#[tokio::test]
async fn an_unsaturated_session_is_refused_with_its_real_utilisation() {
    let (app, state, _tmp) = make_app().await;
    let session = state
        .session_storage
        .create_session("p7-unsaturated".to_string())
        .await
        .expect("create session");
    seed_history(&state, &session.id, 12).await;
    // A real turn, comfortably inside the window.
    state.context_monitor.record_turn(&session.id, 800, 8192);

    let body = compact(&app, &session.id).await;
    assert_eq!(body["status"], "skipped", "{body}");
    assert_eq!(body["reason"], "not_under_pressure", "{body}");
    assert_eq!(body["context"]["should_compact"], false, "{body}");
    let pct = body["context"]["utilization_pct"].as_f64().unwrap();
    assert!(
        (9.0..10.5).contains(&pct),
        "the report did not carry the session's real utilisation: {body}",
    );

    // And it must not have burned the claim on the way to refusing: the session
    // is still eligible the moment it does come under pressure.
    state.context_monitor.record_turn(&session.id, 7000, 8192);
    assert!(
        state.context_monitor.claim_compaction(&session.id),
        "refusing an unsaturated session spent the compaction cooldown, so the \
         first real pass would be refused too",
    );
}

/// A typo must not be reported as a healthy window.
#[tokio::test]
async fn compacting_an_unknown_session_is_a_404() {
    let (app, _state, _tmp) = make_app().await;
    let resp = app
        .oneshot(compact_request("no-such-session"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Turning compaction off turns this off too. The manual axis reads the same
/// switch the time and pressure axes read; acting here would resurrect half of a
/// feature the user switched off.
#[tokio::test]
async fn the_hybrid_compaction_switch_disables_the_manual_axis() {
    let (app, state, _tmp) = make_app().await;
    let session = state
        .session_storage
        .create_session("p7-switch".to_string())
        .await
        .expect("create session");
    seed_history(&state, &session.id, 12).await;
    saturate(&state, &session.id);

    let mut settings = state.settings_repo.get().await.expect("read settings");
    settings.hybrid_compaction_enabled = false;
    state
        .settings_repo
        .update(&settings)
        .await
        .expect("save settings");

    let body = compact(&app, &session.id).await;
    assert_eq!(body["status"], "skipped", "{body}");
    assert_eq!(body["reason"], "compaction_disabled", "{body}");
    assert!(
        state.context_monitor.claim_compaction(&session.id),
        "a session refused because the feature is off had its cooldown spent \
         anyway, so switching compaction back on would not restore the button",
    );
}

/// With the monitor off, nothing ever calls `record_turn`, so every utilisation
/// number the endpoint could report is a zero that means "not measured". Saying
/// `not_under_pressure` there would be a confident lie about a session nobody
/// measured; the reason has to name the switch instead.
#[tokio::test]
async fn a_disabled_monitor_is_reported_as_such_not_as_an_empty_window() {
    let (app, state, _tmp) = make_app().await;
    let session = state
        .session_storage
        .create_session("p7-monitor-off".to_string())
        .await
        .expect("create session");
    seed_history(&state, &session.id, 12).await;

    let mut settings = state.settings_repo.get().await.expect("read settings");
    settings.context_monitor_enabled = false;
    state
        .settings_repo
        .update(&settings)
        .await
        .expect("save settings");

    let body = compact(&app, &session.id).await;
    assert_eq!(body["status"], "skipped", "{body}");
    assert_eq!(body["reason"], "monitor_disabled", "{body}");
}
