//! Booking a ride from a member's own paired phone, against a stand-in ride company: who the
//! member is, the fare-then-confirm order, and that one member never touches another's ride.

use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_api::build_router;
use pond_core::rides::booking::RideBooking;
use pond_core::rides::mocks::MockRideProvider;
use pond_core::user_data::domain::profile::CreateProfileRequest;
use serde_json::{json, Value};
use tower::ServiceExt;

static CREDENTIAL: LazyLock<pond_api::host_guard::HostCredential> =
    LazyLock::new(pond_api::host_guard::HostCredential::generate);

/// One stand-in company for the whole binary: `rides::install` is process-wide.
static PROVIDER: LazyLock<Arc<MockRideProvider>> = LazyLock::new(|| {
    let provider = Arc::new(MockRideProvider::new());
    pond_api::rides::install(Arc::new(RideBooking::new(provider.clone())));
    provider
});

struct Pond {
    loopback: axum::Router,
    lan: axum::Router,
    profiles: Arc<dyn pond_core::user_data::ports::profile::ProfileRepository + Send + Sync>,
    _dir: tempfile::TempDir,
}

async fn pond() -> Pond {
    LazyLock::force(&PROVIDER);
    let pond_api::test_support::TestState { state, dir, .. } =
        pond_api::test_support::app_state().await;
    let profiles = state.profile_repo.clone();
    let state = Arc::new(state);
    let dist = std::path::PathBuf::from("pond-desktop/dist");
    let router = |ip: [u8; 4]| {
        build_router(state.clone(), dist.clone())
            .layer(axum::Extension(CREDENTIAL.clone()))
            .layer(MockConnectInfo(SocketAddr::from((ip, 40_000))))
    };
    Pond {
        loopback: router([127, 0, 0, 1]),
        lan: router([192, 168, 1, 44]),
        profiles,
        _dir: dir,
    }
}

async fn send(
    router: &axum::Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(pond_api::host_guard::CREDENTIAL_HEADER, CREDENTIAL.as_str());
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let req = match body {
        Some(b) => req
            .header("Content-Type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn client_mac(code: &str, challenge_b64: &str, client_id: &str) -> String {
    use base64::Engine as _;
    use hmac::Mac as _;
    let challenge = base64::engine::general_purpose::STANDARD
        .decode(challenge_b64.as_bytes())
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

/// A member, and a phone paired to them through a pairing code that names them.
async fn member_with_phone(p: &Pond, name: &str) -> String {
    let profile_id = p
        .profiles
        .create(CreateProfileRequest {
            display_name: name.to_string(),
            avatar_emoji: "*".to_string(),
        })
        .await
        .unwrap()
        .id;
    let (status, issued) = send(
        &p.loopback,
        Method::POST,
        "/api/v1/handshake/pairing-code",
        None,
        Some(json!({"profile_id": profile_id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{issued}");
    let code = issued["code"].as_str().unwrap().to_string();
    let client_id = format!("{name}-phone");
    let (_, init) = send(
        &p.loopback,
        Method::POST,
        "/api/v1/handshake/init",
        None,
        Some(json!({"client_id": client_id, "client_type": "gotg", "client_version": "1.0.0"})),
    )
    .await;
    let (_, verified) = send(
        &p.loopback,
        Method::POST,
        "/api/v1/handshake/verify",
        None,
        Some(json!({
            "challenge_id": init["challenge_id"],
            "mac": client_mac(&code, init["challenge"].as_str().unwrap(), &client_id),
            "device_name": client_id,
        })),
    )
    .await;
    verified["session_token"].as_str().unwrap().to_string()
}

fn trip() -> Value {
    json!({
        "pickup": {"latitude": -1.2676, "longitude": 36.8108},
        "dropoff": {"name": "JKIA", "latitude": -1.319167, "longitude": 36.9275},
    })
}

#[tokio::test]
async fn a_member_gets_a_fare_confirms_and_cancels_from_their_phone() {
    let p = pond().await;
    let phone = member_with_phone(&p, "liz").await;
    let before = PROVIDER.requests();

    let (status, quoted) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(trip()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{quoted}");
    assert_eq!(quoted["fare"]["display"], "KES 1,250");
    assert_eq!(quoted["pickup"]["name"], "Pickup");
    assert_eq!(quoted["state"]["state"], "awaiting_confirmation");
    assert_eq!(PROVIDER.requests(), before, "a quote booked a ride");
    let id = quoted["id"].as_str().unwrap();

    let (status, confirmed) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/confirm"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmed}");
    assert_eq!(confirmed["state"]["state"], "requested");
    assert_eq!(PROVIDER.requests(), before + 1);

    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/confirm"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a second confirm was accepted"
    );
    assert_eq!(PROVIDER.requests(), before + 1);

    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/cancel"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn another_members_phone_cannot_see_confirm_or_cancel_the_ride() {
    let p = pond().await;
    let liz = member_with_phone(&p, "liz2").await;
    let jerry = member_with_phone(&p, "jerry").await;
    let (_, quoted) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&liz),
        Some(trip()),
    )
    .await;
    let id = quoted["id"].as_str().unwrap();

    for (method, path) in [
        (Method::GET, format!("/api/v1/rides/{id}")),
        (Method::POST, format!("/api/v1/rides/{id}/confirm")),
        (Method::POST, format!("/api/v1/rides/{id}/decline")),
        (Method::POST, format!("/api/v1/rides/{id}/cancel")),
    ] {
        let (status, _) = send(&p.lan, method, &path, Some(&jerry), None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{path} answered another member"
        );
    }
    let (status, still) = send(
        &p.lan,
        Method::GET,
        &format!("/api/v1/rides/{id}"),
        Some(&liz),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(still["state"]["state"], "awaiting_confirmation");
}

#[tokio::test]
async fn a_declined_fare_cannot_then_be_confirmed() {
    let p = pond().await;
    let phone = member_with_phone(&p, "liz3").await;
    let (_, quoted) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(trip()),
    )
    .await;
    let id = quoted["id"].as_str().unwrap();
    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/decline"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/rides/{id}/confirm"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn only_a_phone_paired_to_a_member_can_book_and_only_on_the_map() {
    let p = pond().await;
    // No device on the request: the pond's own desktop is not a member's phone.
    let (status, _) = send(
        &p.loopback,
        Method::POST,
        "/api/v1/rides/quote",
        None,
        Some(trip()),
    )
    .await;
    assert!(
        matches!(status, StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED),
        "{status}"
    );

    let phone = member_with_phone(&p, "liz4").await;
    let (status, _) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(json!({"pickup": {"latitude": 0.0, "longitude": 0.0}, "dropoff": {"latitude": 91.0, "longitude": 0.0}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_drop_off_far_from_the_pickup_is_never_quoted() {
    let p = pond().await;
    let phone = member_with_phone(&p, "liz5").await;
    let (status, refused) = send(
        &p.lan,
        Method::POST,
        "/api/v1/rides/quote",
        Some(&phone),
        Some(json!({
            "pickup": {"latitude": -1.2676, "longitude": 36.8108},
            "dropoff": {"name": "Westlands", "latitude": 18.03, "longitude": -76.79},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused["error"].as_str().unwrap_or("").contains("too far"),
        "{refused}"
    );
}
