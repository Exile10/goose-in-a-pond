//! Transport boundary tests use real SQLite challenges and never trust forwarding headers.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};

use pond_infra::sqlite_event_log::SqliteEventLog;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use serde_json::Value;
use tower::ServiceExt;

struct Harness {
    loopback: axum::Router,
    remote: axum::Router,
    handshake: Arc<SqliteHandshakeAdapter>,
    companion: axum::Router,
    state: Arc<AppState>,
    event_log: Arc<SqliteEventLog>,
    _tmp: tempfile::TempDir,
}

async fn make_app() -> Harness {
    let pond_api::test_support::TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    // A real event log, so a test can read back the audit event a pairing records.
    let event_log = Arc::new(SqliteEventLog::new(state.db.logs.clone()));
    state.event_log = Some(event_log.clone());
    let state = Arc::new(state);

    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let loopback = build_router(state.clone(), dist.clone())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let companion = pond_api::build_companion_router(state.clone());
    let remote = build_router(state.clone(), dist).layer(MockConnectInfo(SocketAddr::from((
        [100, 64, 0, 44],
        40_000,
    ))));

    Harness {
        loopback,
        remote,
        handshake,
        companion,
        state,
        event_log,
        _tmp: dir,
    }
}

use pond_api::test_support::{pair_phone, TEST_SPKI_PIN};
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
        (
            "/api/v1/handshake/pake/init",
            json!({"client_id":"remote", "client_type":"gotg", "client_version":"test",
                   "channel_binding": TEST_SPKI_PIN, "client_nonce": "AAAA"}),
        ),
        (
            "/api/v1/handshake/pake/verify",
            json!({"challenge_id":"none", "share":"AAAA", "confirm":"00"}),
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

    // The same for a CPace run: started on loopback, it cannot be finished over the tailnet.
    h.handshake.issue_pairing_code().await.unwrap();
    let (status, challenge) = post(
        &h.loopback,
        "/api/v1/handshake/pake/init",
        json!({"client_id":"phone", "client_type":"gotg", "client_version":"test",
               "channel_binding": TEST_SPKI_PIN,
               "client_nonce": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let (status, body) = post(
        &h.remote,
        "/api/v1/handshake/pake/verify",
        json!({"challenge_id": challenge["challenge_id"], "share": challenge["share"],
               "confirm": "00"}),
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
    let (status, body) = pair_phone(&h.companion, "123456", "phone", TEST_SPKI_PIN).await;
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

/// A phone on the LAN pairs with CPace, and only with CPace: the dashboard's HMAC exchange and
/// the legacy single step are this host's alone.
#[tokio::test]
async fn a_phone_on_the_lan_pairs_only_with_cpace() {
    let Some(address) = own_lan_address() else {
        eprintln!("SKIPPED: this host has no attached LAN, so no LAN peer can be simulated");
        return;
    };
    let h = make_app().await;
    let lan = h
        .companion
        .clone()
        .layer(MockConnectInfo(SocketAddr::new(address, 40_000)));
    let code = h.handshake.issue_pairing_code().await.unwrap().code;

    let init = json!({"client_id":"lan-phone", "client_type":"gotg", "client_version":"test"});
    let (status, body) = post(&lan, "/api/v1/handshake/init", init.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "pairing_requires_cpace");

    // A loopback challenge completed from the LAN is refused without being spent.
    let (status, challenge) = post(&h.loopback, "/api/v1/handshake/init", init).await;
    assert_eq!(status, StatusCode::OK);
    let mac = unbound_mac(&code, challenge["challenge"].as_str().unwrap(), "lan-phone");
    let verify = json!({"challenge_id": challenge["challenge_id"], "mac": mac});
    let (status, body) = post(&lan, "/api/v1/handshake/verify", verify.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "pairing_requires_cpace");
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

    // A phone that pinned some other key is turned away before any run starts...
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let other = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    let (status, body) = pair_phone(&lan, &code, "lan-phone", other).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "channel_binding_mismatch");
    // ...and one that pinned this Pond's pairs.
    let (status, body) = pair_phone(&lan, &code, "lan-phone", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], true, "{body}");
}

/// A phone on the companion router as a loopback peer, so the LAN guard passes on any host.
fn on_loopback(router: axum::Router) -> axum::Router {
    router.layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_001))))
}

/// The original unbound transcript, `challenge || client_id`, keyed by the pairing code: what a
/// client with no TLS key to bind (Expo Go over the plaintext listener) sends.
fn unbound_mac(code: &str, challenge_b64: &str, client_id: &str) -> String {
    use base64::Engine as _;
    use hmac::Mac as _;
    let challenge = base64::engine::general_purpose::STANDARD
        .decode(challenge_b64)
        .unwrap();
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(code.as_bytes()).unwrap();
    mac.update(&challenge);
    mac.update(client_id.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every refusal names its code in `error`, which is what the app reads: a wrong code is
/// `invalid_confirmation` until the code's ten attempts are spent, then `code_retired`.
#[tokio::test]
async fn cpace_refusals_carry_their_code_and_the_budget_retires_the_code() {
    let h = make_app().await;
    // Its own loopback address, so these ten verifies spend nobody else's per-IP allowance.
    let phone = h
        .companion
        .clone()
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 7], 40_003))));
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let wrong = if code == "000000" { "000001" } else { "000000" };

    for attempt in 1..=10 {
        let (status, body) = pair_phone(&phone, wrong, "guesser", TEST_SPKI_PIN).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a refusal is a 200 carrying a reason: {body}"
        );
        assert_eq!(body["accepted"], false);
        let expected = if attempt < 10 {
            "invalid_confirmation"
        } else {
            "code_retired"
        };
        assert_eq!(body["error"], expected, "attempt {attempt}: {body}");
        assert_eq!(body["rejection_reason"], expected);
        assert!(
            body["server_proof"].is_null(),
            "a refusal must not carry a proof"
        );
    }
    let (status, body) = pair_phone(&phone, &code, "guesser", TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "no_active_pairing_code");
}

/// Pair with CPace through `router` and return the audit event's `transport` attribute.
async fn pair_and_audit(h: &Harness, router: &axum::Router, client_id: &str) -> Option<String> {
    let code = h.handshake.issue_pairing_code().await.unwrap().code;
    let (status, body) = pair_phone(router, &code, client_id, TEST_SPKI_PIN).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], true, "{body}");

    use pond_core::security::domain::event::{AttributeValue, EventCategory, EventQuery};
    use pond_core::security::ports::event_log::EventLog;
    let events = h
        .event_log
        .query(EventQuery {
            category: Some(EventCategory::Auth),
            ..Default::default()
        })
        .await
        .unwrap();
    let event = events
        .iter()
        .find(|e| {
            e.action == "auth.device_paired"
                && e.attributes.get("device_name") == Some(&AttributeValue::Text(client_id.into()))
        })
        .expect("the pairing is audited");
    assert_eq!(
        event.attributes.get("method"),
        Some(&AttributeValue::Text("cpace".into()))
    );
    match event.attributes.get("transport") {
        Some(AttributeValue::Text(transport)) => Some(transport.clone()),
        None => None,
        Some(other) => panic!("unexpected transport attribute {other:?}"),
    }
}

/// Phones pair with CPace on the plaintext development listener too, and the audit trail
/// records that they did it there. The dashboard's HMAC exchange is not served to them.
#[tokio::test]
async fn pairing_on_the_insecure_listener_is_cpace_and_audited_as_plaintext() {
    use pond_api::insecure_dev::{advertise, router, InsecureDevLan};
    let h = make_app().await;
    let lan = Some(InsecureDevLan { port: 4080 });
    let https = on_loopback(advertise(h.companion.clone(), lan));
    let insecure = on_loopback(router(advertise(h.companion.clone(), lan)));

    assert_eq!(
        pair_and_audit(&h, &insecure, "expo-go").await.as_deref(),
        Some("insecure_dev")
    );
    assert_eq!(pair_and_audit(&h, &https, "release-phone").await, None);

    let (status, body) = post(
        &insecure,
        "/api/v1/handshake/init",
        json!({"client_id":"expo-go", "client_type":"gotg", "client_version":"test"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "pairing_requires_cpace");
}

async fn system_info(router: &axum::Router, bearer: Option<&str>) -> Value {
    let mut request = Request::builder().uri("/api/v1/system/info");
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn system_info_names_the_insecure_listener_only_while_it_runs() {
    use pond_api::insecure_dev::{advertise, router, InsecureDevLan};
    let h = make_app().await;
    let token = session_token(&h).await;
    let lan = Some(InsecureDevLan { port: 4080 });
    let off = on_loopback(advertise(
        pond_api::build_companion_router(h.state.clone()),
        None,
    ));
    let https = on_loopback(advertise(h.companion.clone(), lan));
    let insecure = on_loopback(router(advertise(h.companion.clone(), lan)));

    assert_eq!(
        system_info(&off, None).await["protocol"],
        3,
        "anonymous callers learn which pairing protocol this Pond speaks"
    );
    for bearer in [None, Some(token.as_str())] {
        let info = system_info(&off, bearer).await;
        assert!(info.get("insecure_dev").is_none(), "{info}");
        for on in [&https, &insecure] {
            assert_eq!(system_info(on, bearer).await["insecure_dev"], true);
        }
    }
}

#[tokio::test]
async fn a_loopback_peer_on_the_plaintext_listener_is_not_the_host() {
    use pond_api::insecure_dev::{advertise, router, InsecureDevLan};
    let h = make_app().await;
    let lan = Some(InsecureDevLan { port: 4080 });
    // The middleware's refusal, as opposed to the handler's own internal-token check.
    let refused_by_middleware = |app: axum::Router| async move {
        let response = app
            .oneshot(
                Request::get("/api/v1/player/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        body["error"] == "Missing Authorization header"
    };
    // A host-only route: the host process gets past the middleware without a token...
    assert!(!refused_by_middleware(from_this_machine(h.companion.clone())).await);
    // ...but whatever reaches the plaintext door from this machine is not the host.
    let insecure = from_this_machine(router(advertise(h.companion.clone(), lan)));
    assert!(refused_by_middleware(insecure).await);
}

/// A loopback peer as the server records it. `MockConnectInfo` feeds only the extractor, and the
/// auth middleware reads the extension itself, so under the mock no caller is ever the host.
fn from_this_machine(router: axum::Router) -> axum::Router {
    router.layer(axum::Extension(axum::extract::ConnectInfo(
        SocketAddr::from(([127, 0, 0, 1], 40_002)),
    )))
}
