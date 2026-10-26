//! Camera consent and the camera image token, through the real router and auth middleware.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use pond_api::test_support::{pair_phone, TestState, TEST_SPKI_PIN};
use pond_core::security::ports::handshake::Handshake;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn call(
    router: &axum::Router,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = match body {
        Some(v) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    // Refusals are small JSON; a stream that was wrongly opened would not end, so cap the read.
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        axum::body::to_bytes(response.into_body(), 1 << 20),
    )
    .await
    .map(|b| b.unwrap_or_default())
    .unwrap_or_default();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A router over a fresh pond and a paired phone's token.
async fn pond(cameras_enabled: bool) -> (axum::Router, String, tempfile::TempDir) {
    let TestState {
        state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    state
        .settings_repo
        .set_key("cameras_enabled", cameras_enabled.to_string())
        .await
        .unwrap();
    let lan_ip = pond_api::network::interfaces()
        .unwrap()
        .into_iter()
        .find(pond_api::network::is_lan_interface)
        .map(|i| i.ip())
        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
    let router = pond_api::build_companion_router(Arc::new(state))
        .layer(MockConnectInfo(SocketAddr::new(lan_ip, 40_000)));
    let code = handshake.issue_pairing_code().await.unwrap().code;
    let (status, paired) = pair_phone(&router, &code, "vision-phone", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{paired}");
    let token = paired["session_token"].as_str().unwrap().to_string();
    (router, token, dir)
}

const IMAGE_AND_STREAM_GETS: &[&str] = &[
    "/api/v1/camera/snapshot",
    "/api/v1/camera/porch/snapshot",
    "/api/v1/camera/stream",
    "/api/v1/camera/porch/stream",
    "/api/v1/camera/events/1/snapshot",
    "/api/v1/vision/events/stream",
];

#[tokio::test]
async fn with_camera_consent_off_no_picture_or_stream_is_served() {
    let (router, token, _dir) = pond(false).await;

    for uri in IMAGE_AND_STREAM_GETS {
        let (status, body) = call(&router, Method::GET, uri, Some(&token), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
        assert_eq!(body["reason"], "cameras_disabled", "{uri}");
    }
    let (status, body) = call(
        &router,
        Method::POST,
        "/api/v1/faces/register-from-camera",
        Some(&token),
        Some(json!({"profile_id": "anyone"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["reason"], "cameras_disabled");

    let (status, body) = call(
        &router,
        Method::GET,
        "/api/v1/vision/status",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "consent_off");
    assert_eq!(body["cameras_enabled"], false);
}

#[tokio::test]
async fn with_consent_on_the_same_routes_answer_for_themselves() {
    let (router, token, _dir) = pond(true).await;

    // No camera is running in a test, so "nothing to show" rather than "forbidden".
    for uri in [
        "/api/v1/camera/porch/snapshot",
        "/api/v1/camera/events/1/snapshot",
    ] {
        let (status, body) = call(&router, Method::GET, uri, Some(&token), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
    }
    let (status, body) = call(
        &router,
        Method::POST,
        "/api/v1/faces/register-from-camera",
        Some(&token),
        Some(json!({"profile_id": "anyone"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "no face build in the test pond: {body}"
    );

    let (status, body) = call(&router, Method::GET, "/api/v1/faces", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], false);
    assert!(body["people"].is_array());

    let (status, body) = call(&router, Method::GET, "/api/v1/cameras", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let first = &body["cameras"][0];
    assert_eq!(first["live"], false);
    assert!(first["last_frame_at"].is_null());
    assert!(first["snapshot_url"]
        .as_str()
        .unwrap()
        .ends_with("/snapshot"));
}

fn percent_encode_all(token: &str) -> String {
    token.bytes().map(|b| format!("%{b:02X}")).collect()
}

#[tokio::test]
async fn a_query_token_opens_camera_pictures_and_nothing_else() {
    let (router, token, _dir) = pond(true).await;
    let encoded = percent_encode_all(&token);

    let (status, _) = call(
        &router,
        Method::GET,
        &format!("/api/v1/camera/porch/snapshot?token={encoded}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a percent-encoded query token authenticates a camera picture"
    );

    let (status, _) = call(
        &router,
        Method::GET,
        "/api/v1/camera/porch/snapshot?token=not-a-token",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for uri in ["/api/v1/settings", "/api/v1/faces", "/api/v1/vision/status"] {
        let (status, _) = call(
            &router,
            Method::GET,
            &format!("{uri}?token={token}"),
            None,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri} must not take a query token"
        );
    }
}

#[tokio::test]
async fn vision_bus_changes_are_settings_whichever_door_they_come_through() {
    let (router, token, _dir) = pond(true).await;

    let (status, body) = call(
        &router,
        Method::PUT,
        "/api/v1/vision/config",
        Some(&token),
        Some(json!({"min_event_interval_secs": 0, "enabled_event_types": [" Person "]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["min_event_interval_secs"], 1, "clamped");
    assert_eq!(body["vision_event_types"], json!(["person"]));

    let (_, settings) = call(&router, Method::GET, "/api/v1/settings", Some(&token), None).await;
    assert_eq!(settings["vision_event_cooldown_secs"], 1);
    assert_eq!(settings["vision_event_types"], json!(["person"]));

    let (status, saved) = call(
        &router,
        Method::PUT,
        "/api/v1/settings",
        Some(&token),
        Some(json!({"vision_fps": 30, "vision_classifier_confidence": 0.8, "vision_event_types": []})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["vision_fps"], 5, "stored as the pipeline applies it");

    let (_, config) = call(
        &router,
        Method::GET,
        "/api/v1/vision/config",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(config["classifier_confidence_threshold"], 0.8);
    assert_eq!(config["vision_fps"], 5);
    assert_eq!(
        config["enabled_event_types"],
        json!([]),
        "empty enables every type"
    );
}
