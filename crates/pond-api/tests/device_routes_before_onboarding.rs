//! A phone that claims a pond runs owner setup next, and needs its own device meanwhile: which
//! member it belongs to, its push token, its notification stream. On an A16 against a real pond
//! each was `403 onboarding_required` for the whole of owner setup, the stream about once a second.
//! They take the phone's token before onboarding, never another device's id or no token at all.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::{build_router, AppState};
use pond_core::security::ports::handshake::Handshake;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    loopback: axum::Router,
    lan: axum::Router,
    handshake: Arc<pond_infra::sqlite_handshake::SqliteHandshakeAdapter>,
    _dir: tempfile::TempDir,
}

/// A fresh pond mid-onboarding, with push tokens and notifications stored as on a real one.
async fn fresh_pond() -> Harness {
    let pond_api::test_support::TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    state.skip_onboarding = false;
    state.settings_repo = Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
        state.db.system.clone(),
    ));
    state.push_token_repo = Some(Arc::new(
        pond_infra::sqlite_push_token::SqlitePushTokenRepository::new(state.db.system.clone()),
    ));
    state.notification_queue = Some(Arc::new(
        pond_infra::sqlite_notification_queue::SqliteNotificationQueue::new(
            state.db.system.clone(),
        ),
    ));
    let state: Arc<AppState> = Arc::new(state);
    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let lan = build_router(state, dist).layer(MockConnectInfo(SocketAddr::from((
        [192, 168, 1, 50],
        40_000,
    ))));
    Harness {
        loopback,
        lan,
        handshake,
        _dir: dir,
    }
}

/// Pairs over loopback, as the P4 route tests do: pairing needs a peer on the host's own LAN.
async fn paired(h: &Harness) -> String {
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, body) = pair_phone(&h.loopback, &code, "phone", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session_token"].as_str().unwrap().to_string()
}

async fn respond(
    router: &axum::Router,
    method: Method,
    path: &str,
    body: Option<Value>,
    bearer: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    router
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap()
}

/// For bounded bodies only: the notification stream never ends, so it is read by status alone.
async fn send(
    router: &axum::Router,
    method: Method,
    path: &str,
    body: Option<Value>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let response = respond(router, method, path, body, bearer).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn push_token() -> Option<Value> {
    Some(json!({"token": "ExponentPushToken[before-onboarding]", "platform": "expo"}))
}

#[tokio::test]
async fn a_paired_phone_reaches_its_own_device_during_owner_setup() {
    let h = fresh_pond().await;
    let token = paired(&h).await;

    let (status, me) = send(
        &h.lan,
        Method::GET,
        "/api/v1/devices/self",
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    let device = me["device_id"]
        .as_str()
        .expect("the phone's own id")
        .to_string();

    let own = format!("/api/v1/devices/{device}/push-token");
    let (status, body) = send(&h.lan, Method::POST, &own, push_token(), Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = send(&h.lan, Method::DELETE, &own, None, Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let stream = format!("/api/v1/notifications/stream?device_id={device}");
    let response = respond(&h.lan, Method::GET, &stream, None, Some(&token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream",
        "the phone's stream opens instead of failing once a second"
    );
}

#[tokio::test]
async fn another_devices_id_is_still_refused_before_onboarding() {
    let h = fresh_pond().await;
    let token = paired(&h).await;

    let other = "/api/v1/devices/someone-else/push-token";
    for (method, body) in [(Method::POST, push_token()), (Method::DELETE, None)] {
        let (status, answer) = send(&h.lan, method.clone(), other, body, Some(&token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {answer}");
        assert_eq!(answer["error"], "device_mismatch", "{method}");
    }
    let (status, answer) = send(
        &h.lan,
        Method::GET,
        "/api/v1/notifications/stream?device_id=someone-else",
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    assert_eq!(answer["error"], "device_mismatch");
}

#[tokio::test]
async fn no_token_reaches_them_from_the_lan_or_the_host() {
    let h = fresh_pond().await;
    let routes = [
        (Method::GET, "/api/v1/devices/self", None),
        (
            Method::POST,
            "/api/v1/devices/phone/push-token",
            push_token(),
        ),
        (Method::DELETE, "/api/v1/devices/phone/push-token", None),
    ];
    for router in [&h.lan, &h.loopback] {
        for (method, path, body) in routes.clone() {
            let (status, answer) = send(router, method.clone(), path, body, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path} {answer}");
        }
        let response = respond(
            router,
            Method::GET,
            "/api/v1/notifications/stream?device_id=phone",
            None,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

/// Heartbeat names whatever device id it is given, so it waits for onboarding like the rest.
#[tokio::test]
async fn heartbeat_stays_behind_the_onboarding_guard() {
    let h = fresh_pond().await;
    let token = paired(&h).await;
    let (status, answer) = send(
        &h.lan,
        Method::POST,
        "/api/v1/devices/phone/heartbeat",
        Some(json!({})),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    assert_eq!(answer["error"], "onboarding_required");
}
