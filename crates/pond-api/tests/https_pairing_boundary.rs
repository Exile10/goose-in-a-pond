//! Transport boundary tests use real SQLite challenges and never trust forwarding headers.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::notification::Notification;
use pond_core::shared::mocks::mock_agent::MockAgent;

use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;

use pond_infra::db::Database;
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use serde_json::Value;
use tower::ServiceExt;

struct Harness {
    loopback: axum::Router,
    remote: axum::Router,
    handshake: Arc<SqliteHandshakeAdapter>,
    companion: axum::Router,
    _tmp: tempfile::TempDir,
}

async fn make_app() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();
    let pool = db.system.clone();
    let db = Arc::new(db);

    let profiles = Arc::new(SqliteProfileRepository::new(pool.clone()));

    let handshake = Arc::new(SqliteHandshakeAdapter::new(pool.clone(), None));
    let state = Arc::new(AppState {
        warmup: Default::default(),
        db,
        onboarding_repo: Arc::new(pond_infra::onboarding::SqlxOnboardingRepository::new(
            pool.clone(),
        )),
        // Real adapter, not a mock: the route must reach `issue_pairing_code_for`.
        handshake: handshake.clone(),
        whisper_url: "http://127.0.0.1:9000".into(),
        transcribe_audio: None,
        session_storage: Arc::new(SqliteSessionStorage::new(pool.clone())),
        http_client: reqwest::Client::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".into(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: profiles.clone(),
        device_registry: Arc::new(SqliteDeviceRegistry::new(pool.clone())),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        account_sync: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
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
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
        operational_log: None,
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel::<Notification>(16).0,
        notification_queue: None,
        notification_sender: None,
        runs: Arc::new(pond_api::runs::RunSupervisor::default()),
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        lane: None,
        extraction_status: None,
        suggestion_queue: Arc::new(
            pond_infra::sqlite_suggestion_queue::SqliteSuggestionQueue::new(pool.clone()),
        ),
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
        peer_directory: Arc::new(
            pond_core::mesh::mocks::mock_peer_directory::MockPeerDirectory::new(),
        ),
        credit_ledger: Arc::new(
            pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger::new(),
        ),
        usage_tally: Arc::new(pond_core::mesh::mocks::mock_usage_tally::MockUsageTally::new()),
        mesh_transport: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_provider: Arc::new(tokio::sync::RwLock::new(None)),
        peer_capability_query: Arc::new(tokio::sync::RwLock::new(None)),
        mesh_rebuild: None,
    });

    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let companion = pond_api::build_companion_router(state.clone());
    let remote = build_router(state, dist).layer(MockConnectInfo(SocketAddr::from((
        [100, 64, 0, 44],
        40_000,
    ))));

    Harness {
        loopback,
        remote,
        handshake,
        companion,
        _tmp: tmp,
    }
}

use pond_core::security::ports::handshake::{Handshake, HandshakeRequest};
use serde_json::json;

async fn post(router: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("Content-Type", "application/json")
                .header("X-Forwarded-For", "127.0.0.1")
                .header("Forwarded", "for=192.168.1.2")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn tailnet_cannot_pair_even_with_forged_local_headers() {
    let h = make_app().await;
    for (path, body) in [
        (
            "/api/v1/handshake",
            json!({"client_id":"remote", "client_type":"gotg", "client_version":"test", "pairing_code":"123456"}),
        ),
        (
            "/api/v1/handshake/init",
            json!({"client_id":"remote", "client_type":"gotg", "client_version":"test"}),
        ),
        (
            "/api/v1/handshake/verify",
            json!({"challenge_id":"none", "mac":"00"}),
        ),
    ] {
        let (status, body) = post(&h.remote, path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "pairing_requires_lan");
    }
}

#[tokio::test]
async fn a_locally_started_challenge_cannot_be_completed_remotely() {
    let h = make_app().await;
    let (status, challenge) = post(
        &h.loopback,
        "/api/v1/handshake/init",
        json!({
            "client_id":"phone", "client_type":"gotg", "client_version":"test"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post(
        &h.remote,
        "/api/v1/handshake/verify",
        json!({
            "challenge_id": challenge["challenge_id"], "mac":"00"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "pairing_requires_lan");
}

#[tokio::test]
async fn an_existing_session_can_refresh_from_the_tailnet() {
    let h = make_app().await;
    let code = h.handshake.issue_pairing_code().await.unwrap();
    let paired = h
        .handshake
        .handshake(HandshakeRequest {
            client_id: "phone".into(),
            client_type: "gotg".into(),
            client_version: "test".into(),
            pairing_code: Some(code.code),
        })
        .await
        .unwrap();
    assert!(paired.accepted);
    let (status, refreshed) = post(
        &h.remote,
        "/api/v1/handshake/refresh",
        json!({
            "refresh_token": paired.refresh_token.unwrap()
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(refreshed["accepted"], true);
}

/// The dashboard paths the companion listener must never serve.
const DASHBOARD_PATHS: [&str; 4] = ["/", "/dev/test", "/dev/face", "/assets/index.js"];

async fn get(router: &axum::Router, path: &str, bearer: Option<&str>) -> StatusCode {
    let mut request = Request::builder().uri(path);
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

/// Pair a device the honest way and return its session token.
async fn session_token(h: &Harness) -> String {
    let code = h.handshake.issue_pairing_code().await.unwrap();
    let paired = h
        .handshake
        .handshake(HandshakeRequest {
            client_id: "phone".into(),
            client_type: "gotg".into(),
            client_version: "test".into(),
            pairing_code: Some(code.code),
        })
        .await
        .unwrap();
    assert!(paired.accepted);
    paired.session_token.unwrap()
}

#[tokio::test]
async fn companion_never_serves_dashboard_and_missing_peer_fails_closed() {
    let h = make_app().await;

    // A real token still gets 404: the routes are absent, not merely shadowed by middleware.
    let token = session_token(&h).await;
    for path in DASHBOARD_PATHS {
        assert_eq!(
            get(&h.companion, path, Some(&token)).await,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }

    // Anonymous: non-API paths are `Exposure::HostOnly` and a missing peer isn't loopback, so
    // the token check answers 401 first; every path gets it, so it can't probe for a route.
    for path in DASHBOARD_PATHS {
        assert_eq!(
            get(&h.companion, path, None).await,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    let (status, body) = post(
        &h.companion,
        "/api/v1/handshake/init",
        json!({
            "client_id":"phone", "client_type":"gotg", "client_version":"test"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "pairing_requires_lan");
}

/// This host's own address on an attached LAN: the one non-loopback peer the LAN check
/// admits in-process. `None` on a host with no LAN, where the test says it was skipped.
fn own_lan_address() -> Option<std::net::IpAddr> {
    pond_api::network::interfaces()
        .ok()?
        .into_iter()
        .filter(pond_api::network::is_lan_interface)
        .map(|interface| interface.ip())
        .find(|ip| {
            ip.is_ipv4()
                && pond_api::network::is_lan_peer(
                    *ip,
                    &pond_api::network::interfaces().unwrap_or_default(),
                )
        })
}

#[tokio::test]
async fn a_phone_on_the_lan_must_bind_the_key_it_pinned() {
    let Some(address) = own_lan_address() else {
        eprintln!("SKIPPED: this host has no attached LAN, so no LAN peer can be simulated");
        return;
    };
    let h = make_app().await;
    let state_router = h.companion.clone();
    let lan = state_router.layer(MockConnectInfo(SocketAddr::new(address, 40_000)));
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, challenge) = post(
        &lan,
        "/api/v1/handshake/init",
        json!({"client_id":"lan-phone", "client_type":"gotg", "client_version":"test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    use base64::Engine;
    use hmac::Mac;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(challenge["challenge"].as_str().unwrap())
        .unwrap();
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(code.as_bytes()).unwrap();
    mac.update(&raw);
    mac.update(b"lan-phone");
    let unbound: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let verify = json!({"challenge_id": challenge["challenge_id"], "mac": unbound});

    let (status, body) = post(&lan, "/api/v1/handshake/verify", verify.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "channel_binding_required");

    // The refusal spent nothing: the same challenge still completes from loopback.
    let (status, body) = post(&h.loopback, "/api/v1/handshake/verify", verify).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], true);

    let (status, body) = post(
        &lan,
        "/api/v1/handshake",
        json!({"client_id":"lan-phone", "client_type":"gotg", "client_version":"test", "pairing_code": code}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "legacy_pairing_host_only");
}
