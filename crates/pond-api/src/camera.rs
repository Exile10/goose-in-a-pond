//! Camera and vision routes: live frames, event snapshots, the vision bus and its SSE feed.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use pond_core::shared::ports::event_bus::BusEvent;
use pond_core::user_data::domain::sensor::CameraEvent;
use serde_json::{json, Value};

use crate::AppState;

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
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
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
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;
    // Publish after persisting, so the event carries its DB id.
    if let Some(bus) = &state.event_bus {
        event.id = Some(id);
        bus.publish(BusEvent::Camera(event));
    }
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

pub(crate) async fn list_camera_events(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<CameraQueryParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let camera_id = params.camera_id.as_deref().unwrap_or("default");
    let limit = params.limit.unwrap_or(20).min(100);
    let events = state
        .camera_storage
        .list_events(camera_id, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;
    let list: Vec<Value> = events
        .iter()
        .map(|e| {
            let meta_json: Option<Value> = e
                .metadata
                .as_deref()
                .and_then(|m| serde_json::from_str(m).ok());
            json!({
                "id":             e.id,
                "camera_id":      e.camera_id,
                "event_type":     e.event_type,
                "confidence":     e.confidence,
                "snapshot_path":  e.snapshot_path,
                "metadata":       meta_json,
                "acknowledged":   e.acknowledged,
                "created_at":     e.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "events": list })))
}

pub(crate) async fn acknowledge_camera_event(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state.camera_storage.acknowledge(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
    })?;
    Ok(Json(json!({ "status": "ok" })))
}

pub(crate) async fn list_cameras(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut cams = std::collections::BTreeSet::new();
    for c in pond_adapters_vision::LiveCameraRegistry::global().active_cameras() {
        cams.insert(c);
    }
    if let Ok(settings) = state.settings_repo.get().await {
        if !settings.vision_camera_id.trim().is_empty() {
            cams.insert(settings.vision_camera_id);
        }
    }
    if cams.is_empty() {
        cams.insert("camera-1".to_string());
    }
    let list: Vec<Value> = cams
        .into_iter()
        .map(|id| {
            let is_live = pond_adapters_vision::LiveCameraRegistry::global()
                .get_latest_frame(&id)
                .is_some();
            json!({
                "id": id,
                "name": id,
                "is_live": is_live,
                "snapshot_url": format!("/api/v1/camera/{id}/snapshot"),
                "stream_url": format!("/api/v1/camera/{id}/stream"),
            })
        })
        .collect();
    Ok(Json(json!({ "cameras": list })))
}

pub(crate) async fn get_default_camera_snapshot(
    State(state): State<Arc<AppState>>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    get_camera_snapshot_inner(&state, "camera-1").await
}

pub(crate) async fn get_camera_snapshot(
    State(state): State<Arc<AppState>>,
    Path(camera_id): Path<String>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    get_camera_snapshot_inner(&state, &camera_id).await
}

async fn get_camera_snapshot_inner(
    state: &Arc<AppState>,
    camera_id: &str,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    use axum::response::IntoResponse;

    if let Some(bytes) =
        pond_adapters_vision::LiveCameraRegistry::global().get_latest_frame(camera_id)
    {
        return Ok((
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "image/jpeg"),
                (
                    axum::http::header::CACHE_CONTROL,
                    "no-cache, no-store, must-revalidate",
                ),
            ],
            bytes,
        )
            .into_response());
    }

    if let Ok(events) = state.camera_storage.list_events(camera_id, 20).await {
        for event in events {
            if let Some(path) = event.snapshot_path {
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    return Ok((
                        StatusCode::OK,
                        [
                            (axum::http::header::CONTENT_TYPE, "image/jpeg"),
                            (
                                axum::http::header::CACHE_CONTROL,
                                "no-cache, no-store, must-revalidate",
                            ),
                        ],
                        bytes,
                    )
                        .into_response());
                }
            }
        }
    }

    if let Some(data_dir) = &state.data_dir {
        let snap_dir = data_dir.join("snapshots");
        if let Ok(mut entries) = tokio::fs::read_dir(&snap_dir).await {
            let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jpg") {
                    if let Ok(meta) = entry.metadata().await {
                        if let Ok(mod_time) = meta.modified() {
                            if newest.as_ref().map(|(t, _)| mod_time > *t).unwrap_or(true) {
                                newest = Some((mod_time, path));
                            }
                        }
                    }
                }
            }
            if let Some((_, path)) = newest {
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    return Ok((
                        StatusCode::OK,
                        [
                            (axum::http::header::CONTENT_TYPE, "image/jpeg"),
                            (
                                axum::http::header::CACHE_CONTROL,
                                "no-cache, no-store, must-revalidate",
                            ),
                        ],
                        bytes,
                    )
                        .into_response());
                }
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
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    use axum::response::IntoResponse;

    let event = state
        .camera_storage
        .list_events("camera-1", 100)
        .await
        .ok()
        .and_then(|events| events.into_iter().find(|e| e.id == Some(id)));

    if let Some(event) = event {
        if let Some(path) = event.snapshot_path {
            if let Ok(bytes) = tokio::fs::read(&path).await {
                return Ok((
                    StatusCode::OK,
                    [
                        (axum::http::header::CONTENT_TYPE, "image/jpeg"),
                        (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
                    ],
                    bytes,
                )
                    .into_response());
            }
        }
    }

    Err((
        StatusCode::NOT_FOUND,
        Json(json!({"error": format!("No snapshot found for camera event {id}")})),
    ))
}

pub(crate) async fn get_default_camera_stream() -> axum::response::Response {
    get_camera_stream_inner("camera-1")
}

pub(crate) async fn get_camera_stream(Path(camera_id): Path<String>) -> axum::response::Response {
    get_camera_stream_inner(&camera_id)
}

fn get_camera_stream_inner(camera_id: &str) -> axum::response::Response {
    use axum::response::IntoResponse;

    let mut rx = pond_adapters_vision::LiveCameraRegistry::global().subscribe(camera_id);
    let cam_id = camera_id.to_string();

    let stream = async_stream::stream! {
        if let Some(initial) = pond_adapters_vision::LiveCameraRegistry::global().get_latest_frame(&cam_id) {
            let header = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", initial.len());
            yield Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(header));
            yield Ok(axum::body::Bytes::from(initial));
            yield Ok(axum::body::Bytes::from("\r\n"));
        }
        while let Ok(frame) = rx.recv().await {
            let header = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", frame.len());
            yield Ok(axum::body::Bytes::from(header));
            yield Ok(axum::body::Bytes::copy_from_slice(&frame));
            yield Ok(axum::body::Bytes::from("\r\n"));
        }
    };

    (
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "multipart/x-mixed-replace; boundary=frame",
            ),
            (
                axum::http::header::CACHE_CONTROL,
                "no-cache, no-store, must-revalidate",
            ),
            (axum::http::header::PRAGMA, "no-cache"),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct VisionConfigUpdateRequest {
    vision_enabled: Option<bool>,
    vision_camera_id: Option<String>,
    vision_fps: Option<u32>,
    vision_motion_threshold: Option<f64>,
    min_event_interval_secs: Option<u64>,
    classifier_confidence_threshold: Option<f64>,
    face_detection_enabled: Option<bool>,
    face_on_motion: Option<bool>,
    enabled_event_types: Option<Vec<String>>,
}

pub(crate) async fn get_vision_config(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let bus_cfg = pond_adapters_vision::LiveCameraRegistry::global().bus_config();
    let settings = state.settings_repo.get().await.ok();
    let active_cameras = pond_adapters_vision::LiveCameraRegistry::global().active_cameras();
    let listeners = pond_adapters_vision::LiveCameraRegistry::global().event_listener_count();
    Ok(Json(json!({
        "vision_enabled": settings.as_ref().map(|s| s.vision_enabled).unwrap_or(true),
        "vision_camera_id": settings.as_ref().map(|s| s.vision_camera_id.clone()).unwrap_or_else(|| "camera-1".to_string()),
        "vision_fps": settings.as_ref().map(|s| s.vision_fps).unwrap_or(1),
        "vision_motion_threshold": settings.as_ref().map(|s| s.vision_motion_threshold).unwrap_or(0.03),
        "min_event_interval_secs": bus_cfg.min_event_interval_secs,
        "classifier_confidence_threshold": bus_cfg.classifier_confidence_threshold,
        "face_detection_enabled": bus_cfg.face_detection_enabled,
        "face_on_motion": bus_cfg.face_on_motion,
        "enabled_event_types": bus_cfg.enabled_event_types,
        "supported_signals": pond_adapters_vision::VisionBusConfig::supported_signals(),
        "active_cameras": active_cameras,
        "event_listener_count": listeners,
    })))
}

pub(crate) async fn update_vision_config(
    State(state): State<Arc<AppState>>,
    body: Result<Json<VisionConfigUpdateRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;

    pond_adapters_vision::LiveCameraRegistry::global().update_bus_config(|cfg| {
        if let Some(secs) = req.min_event_interval_secs {
            cfg.min_event_interval_secs = secs.max(1);
        }
        if let Some(conf) = req.classifier_confidence_threshold {
            cfg.classifier_confidence_threshold = conf.clamp(0.05, 0.99);
        }
        if let Some(face_en) = req.face_detection_enabled {
            cfg.face_detection_enabled = face_en;
        }
        if let Some(face_mot) = req.face_on_motion {
            cfg.face_on_motion = face_mot;
        }
        if let Some(types) = req.enabled_event_types {
            if !types.is_empty() {
                cfg.enabled_event_types = types;
            }
        }
    });

    if req.vision_enabled.is_some()
        || req.vision_camera_id.is_some()
        || req.vision_fps.is_some()
        || req.vision_motion_threshold.is_some()
    {
        if let Ok(mut s) = state.settings_repo.get().await {
            if let Some(en) = req.vision_enabled {
                s.vision_enabled = en;
            }
            if let Some(cid) = req.vision_camera_id.filter(|c| !c.trim().is_empty()) {
                s.vision_camera_id = cid;
            }
            if let Some(fps) = req.vision_fps {
                s.vision_fps = fps.clamp(1, 5);
            }
            if let Some(mt) = req.vision_motion_threshold {
                s.vision_motion_threshold = mt.clamp(0.005, 0.50);
            }
            let _ = state.settings_repo.update(&s).await;
        }
    }

    get_vision_config(State(state)).await
}

pub(crate) async fn stream_vision_events_sse() -> axum::response::Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use axum::response::IntoResponse;

    let mut rx = pond_adapters_vision::LiveCameraRegistry::global().subscribe_events();
    let stream = async_stream::stream! {
        while let Ok(ev) = rx.recv().await {
            let meta_json: Option<Value> = ev
                .metadata
                .as_deref()
                .and_then(|m| serde_json::from_str(m).ok());
            let payload = json!({
                "id": ev.id,
                "camera_id": ev.camera_id,
                "event_type": ev.event_type,
                "confidence": ev.confidence,
                "snapshot_path": ev.snapshot_path,
                "metadata": meta_json,
                "created_at": ev.created_at.to_rfc3339(),
            });
            yield Ok::<_, std::convert::Infallible>(Event::default().event("vision_event").data(payload.to_string()));
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
