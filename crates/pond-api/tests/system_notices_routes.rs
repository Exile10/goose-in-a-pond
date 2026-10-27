//! Removing a paired device is a security notice for the household: every other paired phone gets
//! a durable copy, the phone that removed it gets none, and the device removed loses its access
//! before anything is sent. A device with no access to lose is not news. A model warm-up is live
//! status for connected screens, never a notice queued or pushed to a phone.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::notification::{Notification, NotificationSender};
use pond_core::security::ports::handshake::Handshake;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_infra::broadcast_notification_sender::BroadcastNotificationSender;
use pond_infra::sqlite_notification_queue::SqliteNotificationQueue;
use pond_infra::sqlite_notification_recipients::SqliteNotificationRecipients;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Harness {
    loopback: axum::Router,
    lan: axum::Router,
    handshake: Arc<pond_infra::sqlite_handshake::SqliteHandshakeAdapter>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
    state: Arc<AppState>,
    _dir: tempfile::TempDir,
}

/// A pond wired as `main.rs` wires it: the real queue, the real recipients, one channel.
async fn pond() -> Harness {
    let pond_api::test_support::TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    let pool = state.db.system.clone();
    let queue = Arc::new(SqliteNotificationQueue::new(pool.clone()));
    let (tx, _) = tokio::sync::broadcast::channel::<Notification>(64);
    let sender: Arc<dyn NotificationSender> = Arc::new(
        BroadcastNotificationSender::new(tx.clone(), queue.clone(), None)
            .with_household_recipients(Arc::new(SqliteNotificationRecipients::new(pool.clone()))),
    );
    state.notification_tx = tx;
    state.notification_queue = Some(queue);
    state.notification_sender = Some(sender);
    let settings = state.settings_repo.clone();
    let state: Arc<AppState> = Arc::new(state);
    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let lan = build_router(state.clone(), dist).layer(MockConnectInfo(SocketAddr::from((
        [192, 168, 1, 50],
        40_000,
    ))));
    Harness {
        loopback,
        lan,
        handshake,
        settings,
        state,
        _dir: dir,
    }
}

struct Phone {
    token: String,
    device: String,
}

async fn paired(h: &Harness, client_id: &str) -> Phone {
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, body) = pair_phone(&h.loopback, &code, client_id, TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["session_token"].as_str().unwrap().to_string();
    let (status, me) = send(
        &h.lan,
        Method::GET,
        "/api/v1/devices/self",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    Phone {
        token,
        device: me["device_id"].as_str().unwrap().to_string(),
    }
}

async fn send(
    router: &axum::Router,
    method: Method,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The `data.kind` of every notice waiting for `phone`.
async fn pending_kinds(h: &Harness, phone: &Phone) -> Vec<String> {
    let (status, body) = send(
        &h.lan,
        Method::GET,
        "/api/v1/notifications/pending",
        Some(&phone.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["notifications"]
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|n| n["data"]["kind"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn removing_a_phone_tells_every_other_phone_but_the_one_that_removed_it() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    let b = paired(&h, "phone-b").await;
    let c = paired(&h, "phone-c").await;

    let (status, body) = send(
        &h.lan,
        Method::DELETE,
        &format!("/api/v1/devices/{}", b.device),
        Some(&a.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    assert_eq!(
        pending_kinds(&h, &c).await,
        vec!["device_removed".to_string()],
        "phone C did not do this and is told"
    );
    assert!(
        pending_kinds(&h, &a).await.is_empty(),
        "phone A removed it, and is not told what it just did"
    );
    let (status, _) = send(
        &h.lan,
        Method::GET,
        "/api/v1/notifications/pending",
        Some(&b.token),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the removed phone's access is gone before the notice goes out"
    );
}

#[tokio::test]
async fn a_removal_is_told_even_when_system_notices_are_switched_off() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    let b = paired(&h, "phone-b").await;
    let c = paired(&h, "phone-c").await;
    h.settings
        .set_key("system_notices_enabled", "false".to_string())
        .await
        .unwrap();

    let (status, body) = send(
        &h.lan,
        Method::DELETE,
        &format!("/api/v1/devices/{}", b.device),
        Some(&a.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        pending_kinds(&h, &c).await,
        vec!["device_removed".to_string()]
    );
}

#[tokio::test]
async fn removing_a_device_that_held_no_access_is_not_news() {
    let h = pond().await;
    let a = paired(&h, "phone-a").await;
    let c = paired(&h, "phone-c").await;
    let (status, sensor) = send(
        &h.lan,
        Method::POST,
        "/api/v1/devices",
        Some(&a.token),
        Some(json!({"name": "Porch sensor", "device_type": "sensor", "capabilities": []})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{sensor}");

    let (status, body) = send(
        &h.lan,
        Method::DELETE,
        &format!("/api/v1/devices/{}", sensor["id"].as_str().unwrap()),
        Some(&a.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(pending_kinds(&h, &c).await.is_empty());
}

/// Seen on a Jetson: with broadcasts durable per phone, every restart queued and pushed both
/// warm-up notices ("Pond is getting ready", "Pond is ready") to every paired phone.
#[tokio::test]
async fn a_warm_up_reaches_connected_screens_and_is_never_queued_for_a_phone() {
    let h = pond().await;
    let phone = paired(&h, "phone-a").await;
    let before = pending_ids(&h, &phone).await;
    let mut live = h.state.notification_tx.subscribe();

    pond_api::spawn_prefix_prewarm(h.state.clone(), false)
        .await
        .unwrap();

    let mut seen = Vec::new();
    while let Ok(n) = live.try_recv() {
        seen.push(n);
    }
    let warmups: Vec<_> = seen
        .iter()
        .filter(|n| n.data.as_ref().is_some_and(|d| d["kind"] == "pond_warmup"))
        .collect();
    assert!(
        !warmups.is_empty(),
        "connected screens still see the warm-up live: {seen:?}"
    );
    assert!(warmups.iter().all(|n| n.target == "broadcast"));
    // The finished notice is sent from a spawned task; give it the time a push would take.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        pending_ids(&h, &phone).await,
        before,
        "no warm-up notice was queued for the phone"
    );
}

async fn pending_ids(h: &Harness, phone: &Phone) -> Vec<String> {
    let (status, body) = send(
        &h.lan,
        Method::GET,
        "/api/v1/notifications/pending",
        Some(&phone.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["notifications"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect()
}
