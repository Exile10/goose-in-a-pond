//! Pairing outcomes become security notifications and `Auth` events. Failure alerts are
//! debounced process-wide (one per 10 min), so this binary has only one failure test.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::mcp::ports::notification::Notification;
use pond_core::mcp::ports::notification::NotificationSender;
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use pond_core::security::domain::event::{
    AttributeValue, EventCategory, EventQuery, PrivacySensitivity,
};
use pond_core::security::ports::event_log::EventLog;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, VerifyRequest,
};
use pond_infra::broadcast_notification_sender::BroadcastNotificationSender;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_event_log::SqliteEventLog;
use pond_infra::sqlite_notification_queue::SqliteNotificationQueue;
use tower::ServiceExt;

/// Rejects like a real wrong code: a 200 with `accepted: false` and a reason, not a 500.
struct RejectingHandshake;

#[async_trait]
impl Handshake for RejectingHandshake {
    async fn handshake(&self, _request: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("not used in this test")
    }
    async fn validate_token(&self, _token: &str) -> Result<bool> {
        Ok(false)
    }
    async fn revoke_device(&self, _device_id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn revoke_token(&self, _token: &str) -> Result<()> {
        Ok(())
    }
    async fn verify_handshake(&self, _request: VerifyRequest) -> Result<HandshakeResponse> {
        Ok(HandshakeResponse {
            accepted: false,
            session_token: None,
            refresh_token: None,
            expires_at: None,
            hostname: "pond-test".into(),
            server_version: "test".into(),
            capabilities: vec![],
            rejection_reason: Some("invalid_mac".into()),
            server_proof: None,
        })
    }
}

/// Always accepts; `MockHandshake`'s default `verify_handshake` errors instead.
struct AcceptingHandshake;

#[async_trait]
impl Handshake for AcceptingHandshake {
    async fn handshake(&self, _request: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("not used in this test")
    }
    async fn validate_token(&self, _token: &str) -> Result<bool> {
        Ok(false)
    }
    async fn revoke_device(&self, _device_id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn revoke_token(&self, _token: &str) -> Result<()> {
        Ok(())
    }
    async fn verify_handshake(&self, _request: VerifyRequest) -> Result<HandshakeResponse> {
        Ok(HandshakeResponse {
            accepted: true,
            session_token: Some("fresh-session-token".into()),
            refresh_token: Some("fresh-refresh-token".into()),
            expires_at: None,
            hostname: "pond-test".into(),
            server_version: "test".into(),
            capabilities: vec![],
            rejection_reason: None,
            server_proof: None,
        })
    }
}

struct Harness {
    router: axum::Router,
    notifications: tokio::sync::broadcast::Receiver<Notification>,
    event_log: Arc<dyn EventLog>,
    _tmp: tempfile::TempDir,
}

async fn make_app(handshake: Arc<dyn Handshake>) -> Harness {
    let base = pond_api::test_support::app_state().await;
    let (notification_tx, notifications) = tokio::sync::broadcast::channel::<Notification>(64);
    let queue: Arc<dyn NotificationQueueRepository> =
        Arc::new(SqliteNotificationQueue::new(base.state.db.system.clone()));
    let sender: Arc<dyn NotificationSender> = Arc::new(BroadcastNotificationSender::new(
        notification_tx.clone(),
        queue.clone(),
        None,
    ));
    let event_log: Arc<dyn EventLog> = Arc::new(SqliteEventLog::new(base.state.db.logs.clone()));
    let state = Arc::new(AppState {
        handshake,
        event_log: Some(event_log.clone()),
        notification_tx,
        notification_queue: Some(queue),
        notification_sender: Some(sender),
        ..base.state
    });

    // `/handshake/verify` extracts `ConnectInfo` for rate limiting; oneshot has no socket.
    let router = build_router(state, std::path::PathBuf::from("pond-desktop/dist")).layer(
        MockConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 40000))),
    );
    Harness {
        router,
        notifications,
        event_log,
        _tmp: base.dir,
    }
}

async fn post_verify(router: &axum::Router, device_name: &str) -> StatusCode {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/handshake/verify")
        .header("Content-Type", "application/json")
        .body(Body::from(format!(
            r#"{{"challenge_id":"c-1","mac":"00","device_name":"{device_name}"}}"#
        )))
        .unwrap();
    router.clone().oneshot(req).await.unwrap().status()
}

async fn auth_actions(event_log: &Arc<dyn EventLog>) -> Vec<String> {
    event_log
        .query(EventQuery {
            category: Some(EventCategory::Auth),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .collect()
}

#[tokio::test]
async fn successful_pairing_notifies_devices_and_records_an_auth_event() {
    let mut h = make_app(Arc::new(AcceptingHandshake)).await;

    let status = post_verify(&h.router, "Amina's Phone").await;
    assert_eq!(status, StatusCode::OK);

    let n = h
        .notifications
        .try_recv()
        .expect("a notification broadcast");
    assert_eq!(n.category, "info");
    assert_eq!(n.title, "New device paired");
    assert!(n.body.contains("Amina's Phone"), "body: {}", n.body);

    assert_eq!(auth_actions(&h.event_log).await, vec!["auth.device_paired"]);
}

/// All failure-path assertions share one test: only the first failure per binary gets an alert.
#[tokio::test]
async fn failed_pairing_alerts_once_records_why_and_debounces() {
    let mut rejected = make_app(Arc::new(RejectingHandshake)).await;
    assert_eq!(
        post_verify(&rejected.router, "Amina's Phone").await,
        StatusCode::OK,
        "a refusal is a 200 carrying a reason, not an error"
    );

    let n = rejected
        .notifications
        .try_recv()
        .expect("an alert broadcast");
    assert_eq!(n.category, "alert");
    assert_eq!(n.title, "Failed pairing attempt");

    let events = rejected
        .event_log
        .query(EventQuery {
            category: Some(EventCategory::Auth),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "auth.pairing_verify_failed");
    // The log must say why the pairing failed.
    assert_eq!(
        events[0].attributes.get("rejection_reason"),
        Some(&AttributeValue::Text("invalid_mac".into())),
    );
    // Sensitive, so audit tools see it; never Secret, and no MAC or token material.
    assert_eq!(events[0].privacy_sensitivity, PrivacySensitivity::Sensitive);

    // An erroring handshake: still recorded, but no alert since the window is spent.
    let mut errored = make_app(Arc::new(MockHandshake::new())).await;
    assert_eq!(
        post_verify(&errored.router, "intruder").await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(
        errored.notifications.try_recv().is_err(),
        "a burst of guesses must produce one alert per window, not one per guess"
    );
    let actions = auth_actions(&errored.event_log).await;
    assert_eq!(actions, vec!["auth.pairing_verify_failed"]);
}
