//! Transport boundary tests use real SQLite challenges and never trust forwarding headers.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;

use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
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
    let pond_api::test_support::TestState {
        state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    let state = Arc::new(state);

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
        _tmp: dir,
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
