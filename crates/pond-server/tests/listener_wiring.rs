//! Which routes each listener serves. `serve()` binds exactly what `compose()` returns, so
//! these checks cover the production wiring without starting the server.

use std::net::SocketAddr;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use pond_api::network::CompanionTransport;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_server::listeners::{compose, Listeners};
use serde_json::Value;
use tower::ServiceExt;

const LOOPBACK: ([u8; 4], u16) = ([127, 0, 0, 1], 40_000);
const LAN: ([u8; 4], u16) = ([192, 168, 1, 20], 40_000);

/// Accepts one bearer, so a 404 means the route is absent rather than the caller refused.
struct PairedPhone;

#[async_trait]
impl Handshake for PairedPhone {
    async fn revoke_device(&self, _: &str) -> Result<u64> {
        Ok(0)
    }
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not exercised by the wiring tests")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(token == TOKEN)
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        Ok((token == TOKEN).then(|| TokenCaller {
            client_id: "phone".into(),
            device_id: "phone".into(),
        }))
    }
    async fn revoke_token(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

const TOKEN: &str = "wiring-test-token";

struct Harness {
    listeners: Listeners,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let test = pond_api::test_support::app_state().await;
    let transport = CompanionTransport {
        https_port: 4443,
        tls_spki_sha256: "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
    };
    #[cfg(unix)]
    let (embedded, _socket) =
        pond_server::embedded_network::Runtime::new(test.dir.path(), 4000).unwrap();
    let listeners = compose(
        std::sync::Arc::new(pond_api::AppState {
            handshake: std::sync::Arc::new(PairedPhone),
            ..test.state
        }),
        std::path::PathBuf::from("pond-desktop/dist"),
        transport,
        #[cfg(unix)]
        embedded,
    );
    Harness {
        listeners,
        _dir: test.dir,
    }
}

async fn get(router: &axum::Router, peer: ([u8; 4], u16), path: &str) -> (StatusCode, Value) {
    send(
        // The real extension, as `into_make_service_with_connect_info` inserts it: middleware
        // that reads it directly never sees `MockConnectInfo`.
        router
            .clone()
            .layer(axum::Extension(ConnectInfo(SocketAddr::from(peer)))),
        Request::get(path)
            .header("Authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn send(router: axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn remote_access_management_is_served_only_by_the_dashboard() {
    let h = harness().await;
    let (dashboard, _) = get(&h.listeners.dashboard, LOOPBACK, "/api/v1/remote-access").await;
    assert_ne!(
        dashboard,
        StatusCode::NOT_FOUND,
        "the dashboard lost management"
    );
    let (companion, _) = get(&h.listeners.companion, LAN, "/api/v1/remote-access").await;
    assert_eq!(companion, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_companion_serves_no_dashboard_assets() {
    let h = harness().await;
    let (status, _) = get(&h.listeners.companion, LAN, "/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&h.listeners.dashboard, LOOPBACK, "/").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn both_public_listeners_advertise_the_https_transport() {
    let h = harness().await;
    for (router, peer) in [
        (&h.listeners.dashboard, LOOPBACK),
        (&h.listeners.companion, LAN),
    ] {
        let (status, body) = get(router, peer, "/api/v1/system/info").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["https_port"], 4443);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_embedded_socket_admits_only_forwarded_tailnet_peers() {
    let h = harness().await;
    let request = |peer: Option<&str>| {
        let mut request =
            Request::get("/api/v1/health").header("Authorization", format!("Bearer {TOKEN}"));
        if let Some(peer) = peer {
            request = request.header("x-pond-embedded-peer", peer);
        }
        request.body(Body::empty()).unwrap()
    };
    let (status, _) = send(h.listeners.embedded.clone(), request(None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no forwarded peer");
    let (status, _) = send(
        h.listeners.embedded.clone(),
        request(Some("127.0.0.1:1234")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a forwarded peer is never loopback"
    );
    let (status, _) = send(
        h.listeners.embedded.clone(),
        request(Some("100.64.0.9:1234")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        h.listeners.embedded.clone(),
        Request::get("/api/v1/remote-access")
            .header("x-pond-embedded-peer", "100.64.0.9:1234")
            .header("Authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "management reached the tailnet"
    );
}
