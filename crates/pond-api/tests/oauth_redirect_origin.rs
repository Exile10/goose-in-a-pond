//! Where a provider sends the browser back after sign-in: the origin the browser reached the pond
//! at, when that is an http loopback origin with a port (an SSH tunnel's local port, say), and the
//! port the pond bound otherwise. The token exchange must send the same URI the start did.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::security::ports::secret::SecretRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

/// The port the pond bound; the tunnel in these tests forwards 9080 to it.
const BOUND_PORT: u16 = 8080;
const CLIENT_ID: &str = "household-spotify-app";

struct Pond {
    router: axum::Router,
    secrets: Arc<pond_infra::file_secret_repository::FileSecretRepository>,
    state: Arc<pond_api::AppState>,
    _dir: tempfile::TempDir,
}

async fn pond() -> Pond {
    let pond_api::test_support::TestState { state, dir, .. } =
        pond_api::test_support::app_state().await;
    let secrets = Arc::new(
        pond_infra::file_secret_repository::FileSecretRepository::new(dir.path()).unwrap(),
    );
    secrets.set("SPOTIFY_CLIENT_ID", CLIENT_ID).await.unwrap();
    let handshake = Arc::new(pond_infra::mock_handshake::MockHandshake::new());
    handshake.add_valid_token("desktop-token".to_string()).await;
    let state = Arc::new(pond_api::AppState {
        secret_repo: Some(secrets.clone()),
        handshake,
        api_port: BOUND_PORT,
        ..state
    });
    let router = build_router(state.clone(), std::path::PathBuf::from("pond-desktop/dist"))
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    Pond {
        router,
        secrets,
        state,
        _dir: dir,
    }
}

/// `POST /api/v1/oauth/authorize` for Spotify, with an optional `origin` field and `Origin` header.
async fn start(p: &Pond, origin: Option<&str>, origin_header: Option<&str>) -> (StatusCode, Value) {
    let mut body = json!({"provider": "spotify", "extension_id": "music"});
    if let Some(origin) = origin {
        body["origin"] = json!(origin);
    }
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/oauth/authorize")
        .header("Authorization", "Bearer desktop-token")
        .header("Content-Type", "application/json");
    if let Some(origin) = origin_header {
        req = req.header("Origin", origin);
    }
    let resp = p
        .router
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn callback(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v1/oauth/callback")
}

/// Asserts the sign-in started, sent `expected` as its redirect URI, and kept it for the exchange.
async fn assert_redirects_to(p: &Pond, (status, body): (StatusCode, Value), expected: &str) {
    assert_eq!(status, StatusCode::OK, "{body}");
    let auth_url = body["auth_url"].as_str().unwrap();
    let sent = format!("&redirect_uri={}&", urlencoding::encode(expected));
    assert!(auth_url.contains(&sent), "{auth_url}");
    let nonce = body["state"].as_str().unwrap();
    let sessions = p.state.oauth_state.read().await;
    assert_eq!(sessions[nonce].redirect_uri, expected);
}

#[tokio::test]
async fn a_tunnelled_localhost_origin_is_sent_back_through_the_tunnel() {
    let p = pond().await;
    let started = start(&p, Some("http://localhost:9080"), None).await;
    assert_redirects_to(&p, started, &callback(9080)).await;
}

#[tokio::test]
async fn a_loopback_origin_keeps_its_own_port() {
    let p = pond().await;
    let started = start(&p, Some("http://127.0.0.1:4000"), None).await;
    assert_redirects_to(&p, started, &callback(4000)).await;
}

#[tokio::test]
async fn without_a_stated_origin_the_origin_header_is_followed() {
    let p = pond().await;
    let started = start(&p, None, Some("http://localhost:9080")).await;
    assert_redirects_to(&p, started, &callback(9080)).await;
}

#[tokio::test]
async fn a_non_loopback_origin_gets_the_bound_port() {
    let p = pond().await;
    for origin in [
        "http://evil.example:9080",
        "http://192.168.1.20:9080",
        "https://localhost:9080",
    ] {
        let started = start(&p, Some(origin), None).await;
        assert_redirects_to(&p, started, &callback(BOUND_PORT)).await;
        let started = start(&p, None, Some(origin)).await;
        assert_redirects_to(&p, started, &callback(BOUND_PORT)).await;
    }
}

#[tokio::test]
async fn no_origin_gets_the_bound_port() {
    let p = pond().await;
    let started = start(&p, None, None).await;
    assert_redirects_to(&p, started, &callback(BOUND_PORT)).await;
}

#[tokio::test]
async fn the_missing_client_id_message_names_the_uri_this_sign_in_would_send() {
    let p = pond().await;
    p.secrets.delete("SPOTIFY_CLIENT_ID").await.unwrap();
    let (status, body) = start(&p, Some("http://localhost:9080"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "client_id_missing");
    let message = body["error"].as_str().unwrap();
    assert!(message.contains(&callback(9080)), "{message}");
}

/// The callback works from the URI the start kept, not from the port the pond bound.
#[tokio::test]
async fn the_return_uses_the_uri_the_start_kept() {
    let p = pond().await;
    let (status, body) = start(&p, Some("http://localhost:9080"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let nonce = body["state"].as_str().unwrap();

    // With the client ID gone, the callback stops before the network and names the URI it holds.
    p.secrets.delete("SPOTIFY_CLIENT_ID").await.unwrap();
    let resp = p
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/oauth/callback?code=c&state={nonce}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let page = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let page = String::from_utf8_lossy(&page);
    assert!(page.contains(&callback(9080)), "{page}");
    assert!(!page.contains(&callback(BOUND_PORT)), "{page}");
}

/// Collects everything logged while it is the thread's default subscriber.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn starting_a_sign_in_logs_neither_the_client_id_nor_the_state() {
    let p = pond().await;
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);

    let (status, body) = start(&p, Some("http://localhost:9080"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let nonce = body["state"].as_str().unwrap();

    let logged = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("oauth_start"), "{logged}");
    assert!(logged.contains(&callback(9080)), "{logged}");
    assert!(!logged.contains(CLIENT_ID), "{logged}");
    assert!(!logged.contains(nonce), "{logged}");
    assert!(!logged.contains("code_challenge"), "{logged}");
}
