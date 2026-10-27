//! Camera consent and the camera image token, through the real router and auth middleware.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use pond_api::test_support::{pair_phone, TestState, TEST_SPKI_PIN};
use pond_core::security::ports::handshake::Handshake;
use pond_core::user_data::domain::face_recognition::{
    BoundingBox, FaceEmbedding, FaceIdentification,
};
use pond_core::user_data::ports::face_recognition::{FaceRecognition, PairwiseSimilarity};
use pond_core::user_data::services::face_recognition_slot::{
    FaceModelsProgress, FaceRecognitionSlot,
};
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
    pond_with_faces(cameras_enabled, None).await
}

/// As [`pond`], with the face recognition slot a face build would hold.
async fn pond_with_faces(
    cameras_enabled: bool,
    faces: Option<Arc<FaceRecognitionSlot>>,
) -> (axum::Router, String, tempfile::TempDir) {
    let TestState {
        mut state,
        handshake,
        dir,
    } = pond_api::test_support::app_state().await;
    state.face_recognition = faces;
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
    assert!(body["capture"].is_null(), "nothing captures: {body}");
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
    assert_eq!(body["status"], "not_built");
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
        Some(json!({"vision_fps": 60, "vision_classifier_confidence": 0.8, "vision_event_types": []})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["vision_fps"], 30, "stored as the pipeline applies it");

    let (_, config) = call(
        &router,
        Method::GET,
        "/api/v1/vision/config",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(config["classifier_confidence_threshold"], 0.8);
    assert_eq!(config["vision_fps"], 30);
    assert_eq!(
        config["enabled_event_types"],
        json!([]),
        "empty enables every type"
    );
}

#[tokio::test]
async fn vision_status_says_where_each_model_runs() {
    use pond_core::models::domain::acceleration::{self, record_onnx_device, OnnxDevice};
    let (router, token, _dir) = pond(true).await;

    let (status, body) = call(
        &router,
        Method::GET,
        "/api/v1/vision/status",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["faces"]["accelerators"]["embedder"].is_null(),
        "a model this process never loaded has no device: {body}"
    );

    record_onnx_device(acceleration::VISION_CLASSIFIER, OnnxDevice::TensorRt);
    record_onnx_device(acceleration::FACE_DETECTOR, OnnxDevice::Cpu);
    let (_, body) = call(
        &router,
        Method::GET,
        "/api/v1/vision/status",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(body["classifier"]["accelerator"], "tensorrt");
    assert_eq!(body["faces"]["accelerators"]["detector"], "cpu");
}

/// Loaded recognition with nobody enrolled: enough for the routes that only read.
struct NobodyEnrolled;

#[async_trait::async_trait]
impl FaceRecognition for NobodyEnrolled {
    async fn register_face(
        &self,
        _profile_id: &str,
        _image_bytes: &[u8],
        _bbox: Option<BoundingBox>,
    ) -> anyhow::Result<FaceEmbedding> {
        anyhow::bail!("not exercised")
    }
    async fn identify_face(
        &self,
        _image_bytes: &[u8],
        _bbox: Option<BoundingBox>,
    ) -> anyhow::Result<FaceIdentification> {
        anyhow::bail!("not exercised")
    }
    async fn list_embeddings(&self, _profile_id: &str) -> anyhow::Result<Vec<FaceEmbedding>> {
        Ok(Vec::new())
    }
    async fn delete_embeddings(&self, _profile_id: &str) -> anyhow::Result<u64> {
        Ok(0)
    }
    fn match_threshold(&self) -> f32 {
        0.42
    }
    async fn pairwise_similarities(&self) -> anyhow::Result<Vec<PairwiseSimilarity>> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn while_the_face_models_download_face_routes_say_so_and_switch_on_when_they_land() {
    let slot = Arc::new(FaceRecognitionSlot::downloading());
    slot.report_progress(FaceModelsProgress {
        file: "adaface_ir101.onnx".into(),
        downloaded_bytes: 50 * 1_048_576,
        total_bytes: 261 * 1_048_576,
    });
    let (router, token, _dir) = pond_with_faces(true, Some(slot.clone())).await;
    let threshold = "/api/v1/faces/profile/anyone/threshold";

    let (status, body) = call(&router, Method::GET, threshold, Some(&token), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "face_models_downloading", "{body}");
    assert_eq!(body["progress"]["file"], "adaface_ir101.onnx");
    assert_eq!(body["progress"]["downloaded_bytes"], 50 * 1_048_576);
    assert!(body["error"].as_str().is_some_and(|e| !e.is_empty()));

    let (status, body) = call(&router, Method::GET, "/api/v1/faces", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["available"], false);
    assert_eq!(body["status"], "downloading");
    assert_eq!(body["progress"]["total_bytes"], 261 * 1_048_576);

    // The download lands: the same router, with no restart, now serves recognition.
    slot.install(Arc::new(NobodyEnrolled));

    let (status, body) = call(&router, Method::GET, threshold, Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(&router, Method::GET, "/api/v1/faces", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["available"], true);
    assert_eq!(body["status"], "ready");
    assert_eq!(body["default_threshold"].as_f64(), Some(0.42_f32 as f64));
}

#[tokio::test]
async fn a_face_download_that_failed_answers_with_its_reason() {
    let slot = Arc::new(FaceRecognitionSlot::downloading());
    slot.mark_unavailable("No face embedding model could be downloaded.");
    let (router, token, _dir) = pond_with_faces(true, Some(slot)).await;

    let (status, body) = call(
        &router,
        Method::GET,
        "/api/v1/faces/profile/anyone/threshold",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "face_recognition_unavailable", "{body}");
    assert_eq!(
        body["reason"],
        "No face embedding model could be downloaded."
    );
    assert!(body["error"]
        .as_str()
        .is_some_and(|e| e.contains("No face embedding model could be downloaded.")));

    let (status, body) = call(&router, Method::GET, "/api/v1/faces", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "unavailable");
    assert_eq!(
        body["reason"],
        "No face embedding model could be downloaded."
    );
}
