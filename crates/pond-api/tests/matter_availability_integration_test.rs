//! Integration tests for the Matter-unavailable responses.
//!
//! Commissioning used to answer every failure with one sentence — "Matter is not
//! enabled on this Pond — turn it on in Settings first." — which was wrong twice
//! over: there was no such control in Settings, and the message was also what a
//! user saw when the setting was already on and the controller was simply down,
//! or when the binary had no Matter support at all. These tests pin the four
//! causes to four distinct, actionable messages on both Matter routes.
//!
//! Run: cargo test -p pond-api --test matter_availability_integration_test

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_commissioning::{MatterAvailability, MatterUnavailable};
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use reqwest::Client as ReqwestClient;
use std::sync::Arc;
use tower::ServiceExt;

// ── Stubs ─────────────────────────────────────────────────────────────────────

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

/// An app wired with a specific Matter availability, so each startup outcome can
/// be exercised without standing up a controller.
async fn make_app(matter: MatterAvailability) -> (axum::Router, tempfile::TempDir) {
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
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(NoDevices),
        matter,
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
        face_recognition: None,
        session_user_bindings: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
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
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
        weather_provider: None,
    });
    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        tmp,
    )
}

/// The `error` string from a commission attempt, and the status it came with.
async fn commission_error(why: MatterUnavailable) -> (StatusCode, String) {
    let (app, _tmp) = make_app(MatterAvailability::Unavailable(why)).await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/devices/commission")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                // A valid code, so nothing else can be the reason for the refusal.
                .body(Body::from(r#"{"code":"20202021"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (
        status,
        json["error"].as_str().unwrap_or_default().to_string(),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn each_cause_gets_its_own_message_and_none_is_the_old_catch_all() {
    let causes = [
        MatterUnavailable::Disabled,
        MatterUnavailable::NoUrl,
        MatterUnavailable::Unreachable {
            url: "ws://127.0.0.1:5580/ws".to_string(),
        },
        MatterUnavailable::Unsupported,
    ];

    let mut messages = Vec::new();
    for cause in causes {
        let (status, error) = commission_error(cause).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        // Every message says what failed before it says why.
        assert!(
            error.starts_with("Cannot commission a device."),
            "missing the action that failed: {error}"
        );
        // The message this whole change exists to remove: it pointed at a
        // Settings control that did not exist, for a cause it had not checked.
        assert!(
            !error.contains("turn it on in Settings first"),
            "the old catch-all is back: {error}"
        );
        messages.push(error);
    }

    for (i, a) in messages.iter().enumerate() {
        for b in &messages[i + 1..] {
            assert_ne!(a, b, "two causes are indistinguishable to the user");
        }
    }
}

#[tokio::test]
async fn the_switched_off_case_names_the_control_that_now_exists() {
    let (_, error) = commission_error(MatterUnavailable::Disabled).await;
    // The Settings > Extensions > Matter section this change adds.
    assert!(error.contains("Settings > Extensions > Matter"), "{error}");
    assert!(error.contains("restart"), "{error}");
}

#[tokio::test]
async fn a_controller_that_is_down_is_not_reported_as_a_setting_problem() {
    let (_, error) = commission_error(MatterUnavailable::Unreachable {
        url: "ws://10.0.0.7:5580/ws".to_string(),
    })
    .await;

    // Naming the address is the difference between a dead end and a next step.
    assert!(error.contains("ws://10.0.0.7:5580/ws"), "{error}");
    // Telling this user to switch a setting on would be actively misleading.
    assert!(!error.contains("Turn it on"), "{error}");
}

#[tokio::test]
async fn deleting_a_matter_device_reports_the_same_cause_from_its_own_action() {
    let (app, _tmp) = make_app(MatterAvailability::Unavailable(MatterUnavailable::Disabled)).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/devices/matter-1")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let error = json["error"].as_str().unwrap();

    // Its own lead-in, the shared cause — the two routes cannot drift apart.
    assert!(
        error.starts_with("Cannot remove this device from the Matter fabric."),
        "{error}"
    );
    assert!(error.contains("Settings > Extensions > Matter"), "{error}");
}

#[tokio::test]
async fn a_non_matter_device_is_still_deletable_with_matter_off() {
    // The Matter gate is keyed on the `matter-<node_id>` id shape. A plain
    // catalogue entry must not be caught by it.
    let (app, _tmp) = make_app(MatterAvailability::off()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/devices/living-room-pi")
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}
