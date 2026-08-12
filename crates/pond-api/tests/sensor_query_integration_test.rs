//! #90 acceptance: the sensor read-out surface answers correctly, and readings
//! produced by an adapter (not just POSTed over HTTP) become queryable.
//!
//! Wires a real `SqliteSensorStorage` into `AppState` so the aggregate SQL and
//! the time-range bounds run for real rather than through an in-memory mock.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
use pond_core::shared::services::sensor_persisting_event_bus::SensorPersistingEventBus;
use pond_core::user_data::domain::sensor::SensorReading;
use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::MockCameraStorage;
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::sensor_storage::SensorStorage;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::onboarding::SqlxOnboardingRepository;
use pond_infra::sqlite_prompt_extra::SqlitePromptExtraRepository;
use pond_infra::sqlite_prompt_template::SqlitePromptTemplateRepository;
use pond_infra::sqlite_recipe::SqliteRecipeRepository;
use pond_infra::sqlite_sensor::SqliteSensorStorage;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_skill::SqliteSkillRepository;
use serde_json::Value;
use sqlx::{Pool, Sqlite};
use tower::ServiceExt;

/// Everything a test needs to drive the sensor surface: the router, the live
/// bus, the logs pool for seeding rows at chosen times, and the tempdir guard.
struct Harness {
    router: axum::Router,
    bus: Arc<InProcessEventBus>,
    logs: Pool<Sqlite>,
    _tmp: tempfile::TempDir,
}

async fn make_harness() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let logs = db.logs.clone();
    let db = Arc::new(db);

    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let bus = Arc::new(InProcessEventBus::new());

    let state = Arc::new(AppState {
        db,
        onboarding_repo: Arc::new(SqlxOnboardingRepository::new(pool.clone())),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(MockDeviceRegistry),
        commissioner: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        // The real store, so the aggregate SQL and the TEXT range comparison
        // are what the assertions actually exercise.
        sensor_storage: Arc::new(SqliteSensorStorage::new(logs.clone())),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: Some(tmp.path().to_path_buf()),
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
        prompt_template_repo: Some(Arc::new(SqlitePromptTemplateRepository::new(pool.clone()))),
        prompt_extra_repo: Some(Arc::new(SqlitePromptExtraRepository::new(pool.clone()))),
        skill_repo: Some(Arc::new(SqliteSkillRepository::new(pool.clone()))),
        recipe_repo: Some(Arc::new(SqliteRecipeRepository::new(pool.clone()))),
        llamafile_manager: None,
        operational_log: None,
        // The plain bus, exactly as shipped: record_sensor persists inline, so
        // wrapping it here would double-write every POST.
        event_bus: Some(bus.clone() as Arc<dyn EventBus>),
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
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
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
        weather_provider: None,
    });

    let router = build_router(state, std::path::PathBuf::from("pond-desktop/dist"));
    Harness {
        router,
        bus,
        logs,
        _tmp: tmp,
    }
}

/// Seed a reading at a chosen time; `record` always stamps `datetime('now')`.
async fn seed(logs: &Pool<Sqlite>, value: f64, created_at: &str) {
    sqlx::query(
        "INSERT INTO sensor_readings (device_id, sensor_type, value, unit, created_at) \
         VALUES ('bedroom', 'temperature', ?, 'C', ?)",
    )
    .bind(value)
    .bind(created_at)
    .execute(logs)
    .await
    .unwrap();
}

async fn get(router: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Authorization", "Bearer test-token")
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

#[tokio::test]
async fn malformed_since_returns_400() {
    let h = make_harness().await;
    seed(&h.logs, 21.0, "2026-01-01 10:00:00").await;

    let (status, body) = get(
        &h.router,
        "/api/v1/sensors/bedroom?sensor_type=temperature&since=yesterday&agg=avg",
    )
    .await;

    // The bug this guards: an unparseable bound used to widen the query to all
    // of history and return a confident average over the wrong window.
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("since"));
}

#[tokio::test]
async fn malformed_until_returns_400() {
    let h = make_harness().await;

    let (status, body) = get(
        &h.router,
        "/api/v1/sensors/bedroom?sensor_type=temperature&until=not-a-time",
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("until"));
}

#[tokio::test]
async fn unknown_agg_returns_400() {
    let h = make_harness().await;

    let (status, body) = get(
        &h.router,
        "/api/v1/sensors/bedroom?sensor_type=temperature&agg=median",
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("agg"));
}

#[tokio::test]
async fn agg_over_empty_window_is_null_with_zero_count() {
    let h = make_harness().await;
    seed(&h.logs, 21.0, "2026-01-01 10:00:00").await;

    let (status, body) = get(
        &h.router,
        "/api/v1/sensors/bedroom?sensor_type=temperature&agg=min&since=2026-02-01T00:00:00",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    // Previously an empty window folded to infinity and serialized as null with
    // no way to tell it from a real reading; count makes the absence explicit.
    assert!(body["min"].is_null());
    assert_eq!(body["count"], 0);
    assert!(body["unit"].is_null());
}

#[tokio::test]
async fn agg_min_max_avg_over_seeded_window() {
    let h = make_harness().await;
    for (v, ts) in [
        (10.0, "2026-01-01 10:00:00"),
        (20.0, "2026-01-01 11:00:00"),
        (30.0, "2026-01-01 12:00:00"),
    ] {
        seed(&h.logs, v, ts).await;
    }

    for (agg, expected) in [("min", 10.0), ("max", 30.0), ("avg", 20.0)] {
        let (status, body) = get(
            &h.router,
            &format!("/api/v1/sensors/bedroom?sensor_type=temperature&agg={agg}&since=2026-01-01T00:00:00"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body[agg], expected, "wrong {agg}");
        assert_eq!(body["count"], 3);
        assert_eq!(body["unit"], "C");
    }
}

#[tokio::test]
async fn agg_respects_the_requested_window() {
    let h = make_harness().await;
    for (v, ts) in [
        (5.0, "2026-01-01 10:00:00"),
        (15.0, "2026-01-02 10:00:00"),
        (25.0, "2026-01-02 12:00:00"),
    ] {
        seed(&h.logs, v, ts).await;
    }

    let (status, body) = get(
        &h.router,
        "/api/v1/sensors/bedroom?sensor_type=temperature&agg=avg&since=2026-01-02T00:00:00",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["avg"], 20.0);
    assert_eq!(body["count"], 2);
}

#[tokio::test]
async fn history_is_capped_and_flags_truncation() {
    let h = make_harness().await;
    for i in 0..5 {
        seed(&h.logs, f64::from(i), &format!("2026-01-01 1{i}:00:00")).await;
    }

    let (status, body) = get(
        &h.router,
        "/api/v1/sensors/bedroom?sensor_type=temperature&since=2026-01-01T00:00:00&limit=2",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["readings"].as_array().unwrap().len(), 2);
    assert_eq!(body["count"], 2);
    // A truncated series must not read as the whole window.
    assert_eq!(body["truncated"], true);
}

#[tokio::test]
async fn history_that_exactly_fills_the_limit_is_not_truncated() {
    let h = make_harness().await;
    for i in 0..2 {
        seed(&h.logs, f64::from(i), &format!("2026-01-01 1{i}:00:00")).await;
    }

    let (status, body) = get(
        &h.router,
        "/api/v1/sensors/bedroom?sensor_type=temperature&since=2026-01-01T00:00:00&limit=2",
    )
    .await;

    // A complete window that happens to be limit-sized is not truncated.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 2);
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn bus_published_reading_becomes_queryable() {
    let h = make_harness().await;

    // Publish exactly as the Matter bridge does: a bus handle and nothing else.
    let bus = SensorPersistingEventBus::new(
        h.bus.clone() as Arc<dyn EventBus>,
        Arc::new(SqliteSensorStorage::new(h.logs.clone())),
    );
    bus.publish(BusEvent::Sensor(SensorReading {
        device_id: "bedroom".into(),
        sensor_type: "temperature".into(),
        value: 21.5,
        unit: "C".into(),
        recorded_at: chrono::Utc::now(),
    }));

    // #90's acceptance criterion: "what is the temperature in the bedroom?"
    // answered from a reading no one POSTed. Persistence is asynchronous, so
    // poll rather than assume the drain has run.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let (status, body) = get(
            &h.router,
            "/api/v1/sensors/bedroom?sensor_type=temperature&agg=current",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if body["value"] == 21.5 {
            assert_eq!(body["unit"], "C");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "bus-published reading never reached the store: {body}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn http_post_is_written_exactly_once() {
    let h = make_harness().await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/sensors")
        .header("Authorization", "Bearer test-token")
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "device_id": "bedroom",
                "sensor_type": "temperature",
                "value": 21.5,
                "unit": "C",
            }))
            .unwrap(),
        ))
        .unwrap();
    let response = h.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // The standing guard on the double-write question: AppState holds the plain
    // bus precisely so the decorator does not also persist what the handler
    // already wrote.
    let storage = SqliteSensorStorage::new(h.logs.clone());
    let rows = storage
        .get_history("bedroom", "temperature", None, None)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "POSTed reading should be stored once");
}
