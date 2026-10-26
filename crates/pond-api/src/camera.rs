//! Camera and vision routes: live frames, event snapshots, the vision bus and its SSE feed,
//! the supervisor's status, and the household's enrolled faces.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, SecondsFormat, Utc};
use pond_adapters_vision::{
    LiveCameraRegistry, SupervisorStatus, VisionBusConfig, VisionState, LIVE_FRAME_WINDOW,
};
use pond_core::shared::ports::event_bus::BusEvent;
use pond_core::user_data::domain::sensor::CameraEvent;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::domain::vision as vision_policy;
use serde_json::{json, Value};

use crate::AppState;

type Refusal = (StatusCode, Json<Value>);

const NO_STORE: &str = "no-cache, no-store, must-revalidate";

fn registry() -> &'static LiveCameraRegistry {
    LiveCameraRegistry::global()
}

fn internal(e: impl std::fmt::Display) -> Refusal {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": e.to_string()})),
    )
}

fn refuse(status: StatusCode, reason: &str, message: impl Into<String>) -> Refusal {
    (
        status,
        Json(json!({"error": message.into(), "reason": reason})),
    )
}

async fn load_settings(state: &AppState) -> Result<Settings, Refusal> {
    state
        .settings_repo
        .get()
        .await
        .map_err(|e| internal(format!("Failed to load settings: {e}")))
}

/// Read from the stored setting on every request: another process may have just turned it off.
async fn require_camera_consent(state: &AppState) -> Result<Settings, Refusal> {
    let settings = load_settings(state).await?;
    if settings.cameras_enabled {
        Ok(settings)
    } else {
        Err(refuse(
            StatusCode::FORBIDDEN,
            "cameras_disabled",
            "Camera access is turned off in Privacy settings",
        ))
    }
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The one `CameraEvent` shape for the event list and the SSE feed.
pub(crate) fn camera_event_json(e: &CameraEvent) -> Value {
    let metadata: Option<Value> = e
        .metadata
        .as_deref()
        .and_then(|m| serde_json::from_str(m).ok());
    let snapshot_url = match (e.id, &e.snapshot_path) {
        (Some(id), Some(_)) => Some(format!("/api/v1/camera/events/{id}/snapshot")),
        _ => None,
    };
    json!({
        "id":            e.id,
        "camera_id":     e.camera_id,
        "event_type":    e.event_type,
        "confidence":    e.confidence,
        "snapshot_path": e.snapshot_path,
        "snapshot_url":  snapshot_url,
        "metadata":      metadata,
        "acknowledged":  e.acknowledged,
        "created_at":    e.created_at.to_rfc3339(),
    })
}

fn jpeg(bytes: Vec<u8>, cache: &'static str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, cache),
        ],
        bytes,
    )
        .into_response()
}

/// An event's stored JPEG, only from the snapshot directory: `snapshot_path` is client-writable
/// through `POST /camera/events`, so an unchecked path would read any file the pond can.
async fn read_snapshot(state: &AppState, path: &str) -> Option<Vec<u8>> {
    let dir = state.data_dir.as_ref()?.join("snapshots");
    let dir = tokio::fs::canonicalize(&dir).await.ok()?;
    let file = tokio::fs::canonicalize(path).await.ok()?;
    if !file.starts_with(&dir) {
        tracing::warn!(
            path,
            "camera snapshot outside the snapshot directory refused"
        );
        return None;
    }
    tokio::fs::read(&file).await.ok()
}

#[derive(serde::Deserialize)]
pub(crate) struct CameraEventRequest {
    camera_id: String,
    event_type: String,
    confidence: Option<f64>,
    snapshot_path: Option<String>,
    metadata: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct CameraQueryParams {
    camera_id: Option<String>,
    limit: Option<usize>,
}

pub(crate) async fn record_camera_event(
    State(state): State<Arc<AppState>>,
    body: Result<Json<CameraEventRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), Refusal> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;
    let mut event = CameraEvent {
        id: None,
        camera_id: req.camera_id,
        event_type: req.event_type,
        confidence: req.confidence,
        snapshot_path: req.snapshot_path,
        metadata: req.metadata,
        acknowledged: false,
        created_at: chrono::Utc::now(),
    };
    let id = state
        .camera_storage
        .record_event(event.clone())
        .await
        .map_err(internal)?;
    // Publish after persisting, so the event carries its DB id.
    event.id = Some(id);
    registry().publish_event(event.clone());
    if let Some(bus) = &state.event_bus {
        bus.publish(BusEvent::Camera(event));
    }
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

pub(crate) async fn list_camera_events(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<CameraQueryParams>,
) -> Result<Json<Value>, Refusal> {
    let camera_id = match params.camera_id.filter(|c| !c.trim().is_empty()) {
        Some(id) => id,
        None => vision_policy::camera_id(&load_settings(&state).await?),
    };
    let limit = params.limit.unwrap_or(20).min(100);
    let events = state
        .camera_storage
        .list_events(&camera_id, limit)
        .await
        .map_err(internal)?;
    let list: Vec<Value> = events.iter().map(camera_event_json).collect();
    Ok(Json(json!({ "events": list })))
}

pub(crate) async fn acknowledge_camera_event(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, Refusal> {
    state
        .camera_storage
        .acknowledge(id)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "status": "ok" })))
}

pub(crate) async fn list_cameras(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, Refusal> {
    let settings = load_settings(&state).await?;
    let mut cams: std::collections::BTreeSet<String> =
        registry().active_cameras().into_iter().collect();
    cams.insert(vision_policy::camera_id(&settings));
    let list: Vec<Value> = cams
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "live": registry().is_live(&id),
                "last_frame_at": registry().last_frame_at(&id).map(rfc3339),
                "snapshot_url": format!("/api/v1/camera/{id}/snapshot"),
                "stream_url": format!("/api/v1/camera/{id}/stream"),
            })
        })
        .collect();
    Ok(Json(json!({ "cameras": list })))
}

pub(crate) async fn get_default_camera_snapshot(
    State(state): State<Arc<AppState>>,
) -> Result<Response, Refusal> {
    let settings = require_camera_consent(&state).await?;
    camera_snapshot(&state, &vision_policy::camera_id(&settings)).await
}

pub(crate) async fn get_camera_snapshot(
    State(state): State<Arc<AppState>>,
    Path(camera_id): Path<String>,
) -> Result<Response, Refusal> {
    require_camera_consent(&state).await?;
    camera_snapshot(&state, &camera_id).await
}

/// The live frame, else the camera's newest event snapshot.
async fn camera_snapshot(state: &AppState, camera_id: &str) -> Result<Response, Refusal> {
    if let Some(bytes) = registry().get_latest_frame(camera_id) {
        return Ok(jpeg(bytes, NO_STORE));
    }
    if let Ok(events) = state.camera_storage.list_events(camera_id, 20).await {
        for path in events.iter().filter_map(|e| e.snapshot_path.as_deref()) {
            if let Some(bytes) = read_snapshot(state, path).await {
                return Ok(jpeg(bytes, NO_STORE));
            }
        }
    }
    Err((
        StatusCode::NOT_FOUND,
        Json(json!({"error": format!("No snapshot available for camera '{camera_id}'")})),
    ))
}

pub(crate) async fn get_camera_event_snapshot(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Response, Refusal> {
    require_camera_consent(&state).await?;
    let event = state.camera_storage.get_event(id).await.map_err(internal)?;
    if let Some(path) = event.as_ref().and_then(|e| e.snapshot_path.as_deref()) {
        if let Some(bytes) = read_snapshot(&state, path).await {
            return Ok(jpeg(bytes, "private, max-age=86400"));
        }
    }
    Err((
        StatusCode::NOT_FOUND,
        Json(json!({"error": format!("No snapshot found for camera event {id}")})),
    ))
}

pub(crate) async fn get_default_camera_stream(
    State(state): State<Arc<AppState>>,
) -> Result<Response, Refusal> {
    let settings = require_camera_consent(&state).await?;
    Ok(camera_stream(&vision_policy::camera_id(&settings)))
}

pub(crate) async fn get_camera_stream(
    State(state): State<Arc<AppState>>,
    Path(camera_id): Path<String>,
) -> Result<Response, Refusal> {
    require_camera_consent(&state).await?;
    Ok(camera_stream(&camera_id))
}

/// MJPEG; ends when consent is revoked or the camera's feed is dropped.
fn camera_stream(camera_id: &str) -> Response {
    use tokio::sync::broadcast::error::RecvError;

    let mut rx = registry().subscribe(camera_id);
    let initial = registry().get_latest_frame(camera_id);
    let revoked = registry().cameras_revoked();

    let stream = async_stream::stream! {
        tokio::pin!(revoked);
        if let Some(initial) = initial {
            let header = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", initial.len());
            yield Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(header));
            yield Ok(axum::body::Bytes::from(initial));
            yield Ok(axum::body::Bytes::from("\r\n"));
        }
        loop {
            let frame = tokio::select! {
                _ = &mut revoked => break,
                frame = rx.recv() => frame,
            };
            match frame {
                Ok(frame) => {
                    let header = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", frame.len());
                    yield Ok(axum::body::Bytes::from(header));
                    yield Ok(axum::body::Bytes::copy_from_slice(&frame));
                    yield Ok(axum::body::Bytes::from("\r\n"));
                }
                // A slow viewer skips frames rather than losing the stream.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    };

    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "multipart/x-mixed-replace; boundary=frame",
            ),
            (header::CACHE_CONTROL, NO_STORE),
            (header::PRAGMA, "no-cache"),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

pub(crate) async fn stream_vision_events_sse(
    State(state): State<Arc<AppState>>,
) -> Result<Response, Refusal> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio::sync::broadcast::error::RecvError;

    require_camera_consent(&state).await?;
    let mut rx = registry().subscribe_events();
    let revoked = registry().cameras_revoked();
    let stream = async_stream::stream! {
        tokio::pin!(revoked);
        loop {
            let event = tokio::select! {
                _ = &mut revoked => break,
                event = rx.recv() => event,
            };
            match event {
                Ok(ev) => {
                    let payload = camera_event_json(&ev);
                    yield Ok::<_, std::convert::Infallible>(Event::default().event("vision_event").data(payload.to_string()));
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct VisionConfigUpdateRequest {
    vision_enabled: Option<bool>,
    vision_camera_id: Option<String>,
    vision_fps: Option<u32>,
    vision_motion_threshold: Option<f64>,
    #[serde(alias = "vision_event_cooldown_secs")]
    min_event_interval_secs: Option<u64>,
    #[serde(alias = "vision_classifier_confidence")]
    classifier_confidence_threshold: Option<f64>,
    #[serde(alias = "vision_face_recognition_enabled")]
    face_detection_enabled: Option<bool>,
    #[serde(alias = "vision_face_on_motion")]
    face_on_motion: Option<bool>,
    #[serde(alias = "vision_event_types")]
    enabled_event_types: Option<Vec<String>>,
}

impl VisionConfigUpdateRequest {
    /// Applies the request to `s` and names the settings keys it touched.
    fn apply(self, s: &mut Settings) -> HashSet<String> {
        let mut touched = HashSet::new();
        let mut mark = |key: &str| {
            touched.insert(key.to_string());
        };
        if let Some(v) = self.vision_enabled {
            s.vision_enabled = v;
            mark("vision_enabled");
        }
        if let Some(v) = self.vision_camera_id.filter(|c| !c.trim().is_empty()) {
            s.vision_camera_id = v.trim().to_string();
            mark("vision_camera_id");
        }
        if let Some(v) = self.vision_fps {
            s.vision_fps = v;
            mark("vision_fps");
        }
        if let Some(v) = self.vision_motion_threshold {
            s.vision_motion_threshold = v;
            mark("vision_motion_threshold");
        }
        if let Some(v) = self.min_event_interval_secs {
            s.vision_event_cooldown_secs = v;
            mark("vision_event_cooldown_secs");
        }
        if let Some(v) = self.classifier_confidence_threshold {
            s.vision_classifier_confidence = v;
            mark("vision_classifier_confidence");
        }
        if let Some(v) = self.face_detection_enabled {
            s.vision_face_recognition_enabled = v;
            mark("vision_face_recognition_enabled");
        }
        if let Some(v) = self.face_on_motion {
            s.vision_face_on_motion = v;
            mark("vision_face_on_motion");
        }
        if let Some(v) = self.enabled_event_types {
            s.vision_event_types = v;
            mark("vision_event_types");
        }
        vision_policy::normalize_settings(s);
        touched
    }
}

pub(crate) async fn get_vision_config(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, Refusal> {
    let settings = load_settings(&state).await?;
    Ok(Json(vision_config_json(&settings)))
}

fn vision_config_json(settings: &Settings) -> Value {
    let bus = VisionBusConfig::from_settings(settings);
    json!({
        "vision_enabled": settings.vision_enabled,
        "vision_camera_id": vision_policy::camera_id(settings),
        "vision_fps": vision_policy::clamp_fps(settings.vision_fps),
        "vision_motion_threshold": vision_policy::clamp_motion_threshold(settings.vision_motion_threshold),
        "vision_event_cooldown_secs": bus.min_event_interval_secs,
        "vision_classifier_confidence": bus.classifier_confidence_threshold,
        "vision_face_recognition_enabled": bus.face_detection_enabled,
        "vision_face_on_motion": bus.face_on_motion,
        "vision_event_types": bus.enabled_event_types,
        "min_event_interval_secs": bus.min_event_interval_secs,
        "classifier_confidence_threshold": bus.classifier_confidence_threshold,
        "face_detection_enabled": bus.face_detection_enabled,
        "face_on_motion": bus.face_on_motion,
        "enabled_event_types": bus.enabled_event_types,
        "supported_signals": VisionBusConfig::supported_signals(),
        "active_cameras": registry().active_cameras(),
        "event_listener_count": registry().event_listener_count(),
    })
}

pub(crate) async fn update_vision_config(
    State(state): State<Arc<AppState>>,
    body: Result<Json<VisionConfigUpdateRequest>, JsonRejection>,
) -> Result<Json<Value>, Refusal> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;

    let mut settings = load_settings(&state).await?;
    let touched = req.apply(&mut settings);
    if !touched.is_empty() {
        state
            .settings_repo
            .update_fields(&settings, Some(&touched))
            .await
            .map_err(|e| internal(format!("Failed to save settings: {e}")))?;
        if let Err(e) = state.settings_repo.mark_user_set(&touched).await {
            tracing::warn!(error = %e, "failed to record user intent for vision config");
        }
        registry().sync_settings(&settings);
        registry().notify_settings_changed();
    }
    Ok(Json(vision_config_json(&settings)))
}

/// The state to report: the stored switches win over a supervisor that has not caught up yet.
fn reported_state(settings: &Settings, supervisor: &SupervisorStatus) -> VisionState {
    if !settings.cameras_enabled {
        VisionState::ConsentOff
    } else if !settings.vision_enabled {
        VisionState::Off
    } else if matches!(supervisor.state, VisionState::Off | VisionState::ConsentOff) {
        VisionState::Starting
    } else {
        supervisor.state
    }
}

/// How frames reach the pipeline, only while it is capturing them.
fn reported_capture(reported: VisionState, supervisor: &SupervisorStatus) -> Option<&'static str> {
    supervisor
        .capture
        .filter(|_| matches!(reported, VisionState::Starting | VisionState::Running))
        .map(|c| c.as_str())
}

/// `GET /api/v1/vision/status`: what the camera pipeline is doing, and why when it is not.
pub(crate) async fn get_vision_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, Refusal> {
    let settings = load_settings(&state).await?;
    let supervisor = registry().status();
    let reported = reported_state(&settings, &supervisor);
    let capturing = !reported.is_idle();
    let camera_id = supervisor
        .camera_id
        .clone()
        .filter(|_| capturing)
        .unwrap_or_else(|| vision_policy::camera_id(&settings));
    let enrolled_people = match household_faces(&state).await {
        Ok(people) => people.iter().filter(|p| p.enrollments > 0).count(),
        Err(e) => {
            tracing::warn!(error = ?e.1, "vision status could not count enrolled faces");
            0
        }
    };

    Ok(Json(json!({
        "cameras_enabled": settings.cameras_enabled,
        "vision_enabled": settings.vision_enabled,
        "state": reported.as_str(),
        "camera_id": camera_id,
        "source": supervisor.source.as_ref().filter(|_| capturing),
        "source_kind": supervisor.source_kind.filter(|_| capturing).map(|k| k.as_str()),
        "fps": supervisor
            .fps
            .filter(|_| capturing)
            .unwrap_or_else(|| vision_policy::clamp_fps(settings.vision_fps)),
        "capture": reported_capture(reported, &supervisor),
        "last_frame_at": registry().last_frame_at(&camera_id).map(rfc3339),
        "last_event_at": registry().last_event_at().map(rfc3339),
        "last_error": supervisor.last_error.as_ref().filter(|_| capturing),
        "restarts": supervisor.restarts,
        "classifier": { "available": supervisor.classifier_available },
        "faces": {
            "available": state.face_recognition.is_some(),
            "enrolled_people": enrolled_people,
        },
        "event_listeners": registry().event_listener_count(),
    })))
}

struct PersonFaces {
    profile_id: String,
    name: String,
    enrollments: usize,
    threshold: Option<f32>,
    last_enrolled_at: Option<DateTime<Utc>>,
}

/// Every household profile with its enrolment summary; zero enrolments without a face build.
async fn household_faces(state: &AppState) -> Result<Vec<PersonFaces>, Refusal> {
    let profiles = state.profile_repo.list().await.map_err(internal)?;
    let mut people = Vec::with_capacity(profiles.len());
    for profile in profiles {
        let (enrollments, threshold, last_enrolled_at) = match &state.face_recognition {
            Some(face) => {
                let rows = face.list_embeddings(&profile.id).await.map_err(internal)?;
                let threshold = face
                    .get_profile_threshold(&profile.id)
                    .await
                    .map_err(internal)?;
                (
                    rows.len(),
                    threshold,
                    rows.iter().map(|r| r.created_at).max(),
                )
            }
            None => (0, None, None),
        };
        people.push(PersonFaces {
            profile_id: profile.id,
            name: profile.display_name,
            enrollments,
            threshold,
            last_enrolled_at,
        });
    }
    Ok(people)
}

/// `GET /api/v1/faces`: who lives here and how well each is enrolled; never a vector.
pub(crate) async fn list_household_faces(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, Refusal> {
    let people: Vec<Value> = household_faces(&state)
        .await?
        .into_iter()
        .map(|p| {
            json!({
                "profile_id": p.profile_id,
                "name": p.name,
                "enrollments": p.enrollments,
                "threshold": p.threshold,
                "last_enrolled_at": p.last_enrolled_at.map(rfc3339),
            })
        })
        .collect();
    Ok(Json(json!({
        "available": state.face_recognition.is_some(),
        "default_threshold": state.face_recognition.as_ref().map(|f| f.match_threshold()),
        "people": people,
    })))
}

#[derive(serde::Deserialize)]
pub(crate) struct RegisterFromCameraRequest {
    profile_id: String,
    camera_id: Option<String>,
}

/// `POST /api/v1/faces/register-from-camera`: enrol from the pond's own camera, which sees
/// people at the angle and in the light recognition will.
pub(crate) async fn register_face_from_camera(
    State(state): State<Arc<AppState>>,
    body: Result<Json<RegisterFromCameraRequest>, JsonRejection>,
) -> Result<Json<Value>, Refusal> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;
    let settings = require_camera_consent(&state).await?;
    let face = state
        .face_recognition
        .as_ref()
        .ok_or_else(crate::routes::face_unavailable)?;

    match state.profile_repo.get(&req.profile_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("profile {} not found", req.profile_id)})),
            ))
        }
        Err(e) => return Err(internal(format!("profile lookup failed: {e}"))),
    }

    let camera_id = req
        .camera_id
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| vision_policy::camera_id(&settings));
    // Only a fresh frame: an old one may show whoever stood there before.
    let frame = registry()
        .get_fresh_frame(&camera_id, LIVE_FRAME_WINDOW)
        .ok_or_else(|| {
            refuse(
                StatusCode::CONFLICT,
                "no_frame",
                format!("Camera '{camera_id}' has no recent picture; is it watching?"),
            )
        })?;

    let stored = face
        .register_face(&req.profile_id, &frame, None)
        .await
        .map_err(|e| {
            refuse(
                StatusCode::UNPROCESSABLE_ENTITY,
                "no_face",
                format!("{e:#}"),
            )
        })?;

    Ok(Json(json!({
        "id":         stored.id,
        "profile_id": stored.profile_id,
        "model_dims": stored.model_dims,
        "created_at": stored.created_at,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_adapters_vision::CapturePath;

    fn enabled() -> Settings {
        Settings {
            cameras_enabled: true,
            vision_enabled: true,
            ..Settings::default()
        }
    }

    fn supervisor(state: VisionState) -> SupervisorStatus {
        SupervisorStatus {
            state,
            ..SupervisorStatus::default()
        }
    }

    #[test]
    fn the_stored_switches_win_over_the_supervisor() {
        let consent_off = Settings {
            cameras_enabled: false,
            ..enabled()
        };
        assert_eq!(
            reported_state(&consent_off, &supervisor(VisionState::Running)),
            VisionState::ConsentOff,
            "consent off reads as consent_off even before the supervisor stops"
        );
        let both_off = Settings {
            cameras_enabled: false,
            vision_enabled: false,
            ..enabled()
        };
        assert_eq!(
            reported_state(&both_off, &supervisor(VisionState::Off)),
            VisionState::ConsentOff
        );
        let vision_off = Settings {
            vision_enabled: false,
            ..enabled()
        };
        assert_eq!(
            reported_state(&vision_off, &supervisor(VisionState::Running)),
            VisionState::Off
        );
        assert_eq!(
            reported_state(&enabled(), &supervisor(VisionState::Off)),
            VisionState::Starting,
            "switched on, not yet picked up"
        );
        for s in [
            VisionState::NoCamera,
            VisionState::Starting,
            VisionState::Running,
            VisionState::Reconnecting,
        ] {
            assert_eq!(reported_state(&enabled(), &supervisor(s)), s);
        }
    }

    #[test]
    fn the_capture_path_shows_only_while_frames_are_being_captured() {
        let hardware = SupervisorStatus {
            capture: Some(CapturePath::Hardware),
            ..supervisor(VisionState::Running)
        };
        assert_eq!(
            reported_capture(VisionState::Running, &hardware),
            Some("hardware")
        );
        for idle in [
            VisionState::ConsentOff,
            VisionState::Off,
            VisionState::NoCamera,
            VisionState::Reconnecting,
        ] {
            assert_eq!(reported_capture(idle, &hardware), None, "{idle:?}");
        }
        assert_eq!(
            reported_capture(VisionState::Running, &supervisor(VisionState::Running)),
            None
        );
        assert_eq!(CapturePath::CameraJpeg.as_str(), "camera_jpeg");
        assert_eq!(CapturePath::Software.as_str(), "software");
    }

    #[test]
    fn an_event_carries_a_snapshot_url_only_when_it_has_a_snapshot() {
        let mut e = CameraEvent {
            id: Some(7),
            camera_id: "porch".into(),
            event_type: "person".into(),
            confidence: Some(0.9),
            snapshot_path: Some("/data/snapshots/porch-1.jpg".into()),
            metadata: Some(r#"{"face_detected":true,"face_unknown":false}"#.into()),
            acknowledged: false,
            created_at: Utc::now(),
        };
        let v = camera_event_json(&e);
        assert_eq!(v["snapshot_url"], "/api/v1/camera/events/7/snapshot");
        assert_eq!(v["acknowledged"], false);
        assert_eq!(v["metadata"]["face_detected"], true);

        e.snapshot_path = None;
        assert!(camera_event_json(&e)["snapshot_url"].is_null());
        e.snapshot_path = Some("x.jpg".into());
        e.id = None;
        assert!(camera_event_json(&e)["snapshot_url"].is_null());
    }

    #[test]
    fn a_vision_config_update_names_only_the_keys_it_set_and_clamps_them() {
        let req: VisionConfigUpdateRequest = serde_json::from_value(json!({
            "min_event_interval_secs": 0,
            "vision_classifier_confidence": 5.0,
            "enabled_event_types": [],
            "vision_fps": 60,
        }))
        .unwrap();
        let mut s = Settings::default();
        let touched = req.apply(&mut s);
        let mut keys: Vec<_> = touched.into_iter().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "vision_classifier_confidence",
                "vision_event_cooldown_secs",
                "vision_event_types",
                "vision_fps",
            ]
        );
        assert_eq!(s.vision_event_cooldown_secs, 1);
        assert_eq!(s.vision_classifier_confidence, 0.99);
        assert_eq!(s.vision_fps, 30);
        assert!(s.vision_event_types.is_empty(), "empty means every type");
    }
}
