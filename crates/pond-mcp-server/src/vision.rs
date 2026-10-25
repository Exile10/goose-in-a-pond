use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use pond_adapters_vision::LiveCameraRegistry;
use pond_core::user_data::domain::sensor::CameraEvent;
use pond_core::user_data::ports::camera_storage::CameraStorage;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;

const DEFAULT_CAMERA_ID: &str = "camera-1";
const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;
const DEFAULT_WINDOW_FRAMES: usize = 3;
const WINDOW_EVENT_SCAN: usize = 30;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecentCameraEventsParams {
    /// Camera to read (default "camera-1").
    pub camera_id: Option<String>,
    /// Default 10, max 50.
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AcknowledgeCameraEventParams {
    /// Event ID from get_recent_camera_events.
    pub event_id: Option<i64>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct LookAtSnapshotParams {
    /// Camera to look at (default "camera-1").
    pub camera_id: Option<String>,
    /// Specific event from get_recent_camera_events; omit for the newest frame.
    pub event_id: Option<i64>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct LookAtWindowParams {
    /// Camera to look at (default "camera-1").
    pub camera_id: Option<String>,
    /// Frames to sample, evenly spaced across the window. Default 3, max 4.
    pub frames: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SendSnapshotParams {
    /// Camera to look at (default "camera-1").
    pub camera_id: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

async fn snapshot_content(path: &str) -> Option<ContentBlock> {
    use pond_core::models::domain::image_limits::MAX_IMAGE_BYTES;

    let p = std::path::Path::new(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        other => {
            tracing::debug!(
                path,
                ext = other,
                "vision: snapshot is not a readable image"
            );
            return None;
        }
    };

    match tokio::fs::metadata(p).await {
        Ok(m) if m.len() as usize > MAX_IMAGE_BYTES => {
            tracing::warn!(
                path,
                bytes = m.len(),
                "vision: snapshot exceeds per-image limit"
            );
            return None;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::debug!(path, error = %e, "vision: snapshot file is gone");
            return None;
        }
    }

    match tokio::fs::read(p).await {
        Ok(bytes) => Some(ContentBlock::image(
            base64::engine::general_purpose::STANDARD.encode(&bytes),
            mime.to_string(),
        )),
        Err(e) => {
            tracing::warn!(path, error = %e, "vision: could not read snapshot");
            None
        }
    }
}

fn frame_caption(e: &CameraEvent) -> String {
    format!(
        "Frame from '{}' at {} (detector said: {}{})",
        e.camera_id,
        e.created_at.format("%Y-%m-%d %H:%M:%S"),
        e.event_type,
        e.confidence
            .map(|c| format!(", {:.0}% confidence", c * 100.0))
            .unwrap_or_default(),
    )
}

fn pick_evenly_spaced(len: usize, count: usize) -> Vec<usize> {
    if len == 0 || count == 0 {
        return Vec::new();
    }
    if count >= len {
        return (0..len).collect();
    }
    if count == 1 {
        return vec![len - 1];
    }
    let mut picked = Vec::with_capacity(count);
    let step = (len - 1) as f64 / (count - 1) as f64;
    for i in 0..count {
        let idx = (i as f64 * step).round() as usize;
        if !picked.contains(&idx) {
            picked.push(idx);
        }
    }
    picked
}

#[derive(Clone)]
pub struct VisionMcpServer {
    camera_storage: Arc<dyn CameraStorage + Send + Sync>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl VisionMcpServer {
    pub fn new(camera_storage: Arc<dyn CameraStorage + Send + Sync>) -> Self {
        Self {
            camera_storage,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Fixed security cameras only, never an image attached to the message. Text list of recent \
events (motion, pet, package, person), newest first, no frames. Never guess what a camera \
saw.")]
    async fn get_recent_camera_events(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<RecentCameraEventsParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let camera_id = params
            .0
            .camera_id
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_CAMERA_ID.to_string());
        let limit = params
            .0
            .limit
            .map(|l| l as usize)
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);

        match self.camera_storage.list_events(&camera_id, limit).await {
            Ok(events) if events.is_empty() => {
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    crate::format::format_no_results(
                        &format!("recent events from camera '{camera_id}'"),
                        &["giap-vision__look_at_camera_snapshot"],
                    ),
                )]))
            }
            Ok(events) => {
                let lines: Vec<String> = events
                    .iter()
                    .map(|e| {
                        format!(
                            "- [{}] {} {}{}{}",
                            e.id.map(|i| i.to_string()).unwrap_or_else(|| "?".into()),
                            e.created_at.format("%Y-%m-%d %H:%M"),
                            e.event_type,
                            e.confidence
                                .map(|c| format!(" ({:.0}% confidence)", c * 100.0))
                                .unwrap_or_default(),
                            if e.acknowledged {
                                " · acknowledged"
                            } else {
                                ""
                            },
                        )
                    })
                    .collect();
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Recent events from camera '{camera_id}':\n{}",
                    lines.join("\n"),
                ))]))
            }
            Err(e) => {
                tracing::warn!(error = %e, "vision: camera event listing failed");
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    "Sorry, I couldn't read the camera events right now.",
                )]))
            }
        }
    }

    #[tool(description = "\
Fixed security cameras only. One real frame, newest unless event_id is given. If the user \
attached an image, just look at it - do NOT call this.")]
    async fn look_at_camera_snapshot(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<LookAtSnapshotParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let camera_id = params
            .0
            .camera_id
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_CAMERA_ID.to_string());

        if params.0.event_id.is_none() {
            if let Some(live_jpeg) = LiveCameraRegistry::global().get_latest_frame(&camera_id) {
                let img = ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(&live_jpeg),
                    "image/jpeg".to_string(),
                );
                return Ok(CallToolResult::success(vec![
                    ContentBlock::text(format!(
                        "Live snapshot from camera '{camera_id}' at {}.\n\n![Camera Snapshot](/api/v1/camera/{camera_id}/snapshot)",
                        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
                    )),
                    img,
                ]));
            }
        }

        let events = match self
            .camera_storage
            .list_events(&camera_id, WINDOW_EVENT_SCAN)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "vision: camera event listing failed");
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "Sorry, I couldn't reach the camera store right now.",
                )]));
            }
        };

        let chosen = match params.0.event_id {
            Some(id) => events.iter().find(|e| e.id == Some(id)),
            None => events.iter().find(|e| e.snapshot_path.is_some()),
        };

        let Some(event) = chosen else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "No frame available from camera '{camera_id}'."
            ))]));
        };
        let Some(path) = event.snapshot_path.as_deref() else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Event {} has no saved frame.",
                event.id.unwrap_or(-1)
            ))]));
        };
        let Some(image) = snapshot_content(path).await else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "The saved frame for event {} is no longer readable.",
                event.id.unwrap_or(-1)
            ))]));
        };

        let event_id_str = event.id.map(|i| i.to_string()).unwrap_or_default();
        Ok(CallToolResult::success(vec![
            ContentBlock::text(format!(
                "{}\n\n![Camera Snapshot](/api/v1/camera/events/{event_id_str}/snapshot)",
                frame_caption(event)
            )),
            image,
        ]))
    }

    #[tool(description = "\
Fixed security cameras only. Up to 4 frames sampled across time to see what changed. If the \
user attached an image, do NOT call this.")]
    async fn look_at_camera_window(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<LookAtWindowParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let camera_id = params
            .0
            .camera_id
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_CAMERA_ID.to_string());
        let frames = params
            .0
            .frames
            .map(|f| f as usize)
            .unwrap_or(DEFAULT_WINDOW_FRAMES)
            .clamp(1, 4);

        let events = match self
            .camera_storage
            .list_events(&camera_id, WINDOW_EVENT_SCAN)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "vision: camera event listing failed");
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "Sorry, I couldn't reach the camera store right now.",
                )]));
            }
        };

        let mut with_frames: Vec<&CameraEvent> = events
            .iter()
            .filter(|e| e.snapshot_path.is_some())
            .collect();
        with_frames.reverse();
        if with_frames.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "No frames available from camera '{camera_id}'."
            ))]));
        }

        let picked = pick_evenly_spaced(with_frames.len(), frames);
        let mut parts: Vec<ContentBlock> = Vec::with_capacity(picked.len() * 2 + 1);
        parts.push(ContentBlock::text(format!(
            "{} frame(s) from camera '{camera_id}', oldest first, sampled across the last {} events.",
            picked.len(),
            with_frames.len(),
        )));

        for idx in picked {
            let event = with_frames[idx];
            if let Some(path) = event.snapshot_path.as_deref() {
                if let Some(image) = snapshot_content(path).await {
                    parts.push(ContentBlock::text(frame_caption(event)));
                    parts.push(image);
                }
            }
        }

        Ok(CallToolResult::success(parts))
    }

    #[tool(description = "\
Send the live camera picture to the user in this conversation. Use this whenever the user \
asks you to send, show, or share a picture or photo from the camera. Never for an attached image.")]
    async fn send_camera_snapshot(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SendSnapshotParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let camera_id = params
            .0
            .camera_id
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_CAMERA_ID.to_string());

        if let Some(live_jpeg) = LiveCameraRegistry::global().get_latest_frame(&camera_id) {
            let img = ContentBlock::image(
                base64::engine::general_purpose::STANDARD.encode(&live_jpeg),
                "image/jpeg".to_string(),
            );
            return Ok(CallToolResult::success(vec![
                ContentBlock::text(format!(
                    "Here is the picture from camera '{camera_id}':\n\n![Camera Snapshot](/api/v1/camera/{camera_id}/snapshot)"
                )),
                img,
            ]));
        }

        let events = match self
            .camera_storage
            .list_events(&camera_id, WINDOW_EVENT_SCAN)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "vision: camera event listing failed");
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "Sorry, I couldn't reach the camera store right now.",
                )]));
            }
        };

        if let Some(event) = events.iter().find(|e| e.snapshot_path.is_some()) {
            if let Some(path) = event.snapshot_path.as_deref() {
                if let Some(image) = snapshot_content(path).await {
                    let event_id_str = event.id.map(|i| i.to_string()).unwrap_or_default();
                    return Ok(CallToolResult::success(vec![
                        ContentBlock::text(format!(
                            "Here is the picture from camera '{camera_id}':\n\n![Camera Snapshot](/api/v1/camera/events/{event_id_str}/snapshot)"
                        )),
                        image,
                    ]));
                }
            }
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "No snapshot is currently available from camera '{camera_id}'."
        ))]))
    }

    #[tool(description = "\
Acknowledge a camera event by ID to stop re-alerts once the user has seen it.")]
    async fn acknowledge_camera_event(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<AcknowledgeCameraEventParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let Some(event_id) = params.0.event_id else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "I need an `event_id` (see get_recent_camera_events).",
            )]));
        };
        match self.camera_storage.acknowledge(event_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Camera event {event_id} acknowledged."
            ))])),
            Err(e) => {
                tracing::warn!(error = %e, event_id, "vision: acknowledge failed");
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Couldn't acknowledge event {event_id} — check the ID."
                ))]))
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for VisionMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-vision",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Vision MCP server — the FIXED HOME CAMERAS, from the local, on-device \
                 event store (never guess what a camera saw).\n\n\
                 None of these tools are for an image the user attached to a message: an \
                 attachment is already visible to you, so look at it and answer directly.\n\n\
                 Tools: get_recent_camera_events, look_at_camera_snapshot, look_at_camera_window, \
                 send_camera_snapshot, acknowledge_camera_event.",
            )
    }
}

use tokio::io::DuplexStream;

struct VisionDeps {
    camera_storage: Arc<dyn CameraStorage + Send + Sync>,
}

static VISION_DEPS: OnceLock<VisionDeps> = OnceLock::new();

pub fn init_vision_deps(camera_storage: Arc<dyn CameraStorage + Send + Sync>) {
    let _ = VISION_DEPS.set(VisionDeps { camera_storage });
}

pub fn spawn_vision_server(reader: DuplexStream, writer: DuplexStream) {
    let Some(deps) = VISION_DEPS.get() else {
        tracing::error!(
            "giap-vision: init_vision_deps() was never called for this entry point; \
             vision MCP server not started"
        );
        return;
    };
    let server = VisionMcpServer::new(deps.camera_storage.clone());
    crate::serve_builtin("giap-vision", server, reader, writer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use pond_core::user_data::domain::sensor::CameraEvent;

    struct StubStorage(Vec<CameraEvent>);

    #[async_trait]
    impl CameraStorage for StubStorage {
        async fn record_event(&self, _event: CameraEvent) -> Result<i64> {
            Ok(1)
        }
        async fn list_events(&self, camera_id: &str, limit: usize) -> Result<Vec<CameraEvent>> {
            Ok(self
                .0
                .iter()
                .filter(|e| e.camera_id == camera_id)
                .take(limit)
                .cloned()
                .collect())
        }
        async fn acknowledge(&self, _event_id: i64) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn server_constructs() {
        let _server = VisionMcpServer::new(Arc::new(StubStorage(vec![])));
    }

    #[tokio::test]
    async fn a_real_jpeg_becomes_image_content() {
        let dir = std::env::temp_dir().join(format!("giap-vision-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("camera-1-20260728T000000000Z.jpg");
        std::fs::write(&path, b"\xFF\xD8\xFF\xE0 pretend jpeg").unwrap();

        let content = snapshot_content(path.to_str().unwrap())
            .await
            .expect("readable jpeg");
        match content {
            rmcp::model::ContentBlock::Image(img) => {
                assert_eq!(img.mime_type, "image/jpeg");
                assert_eq!(
                    base64::engine::general_purpose::STANDARD
                        .decode(&img.data)
                        .unwrap(),
                    b"\xFF\xD8\xFF\xE0 pretend jpeg"
                );
            }
            other => panic!("expected image content, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_missing_or_non_image_snapshot_yields_nothing() {
        assert!(snapshot_content("/nope/does-not-exist.jpg").await.is_none());

        let dir = std::env::temp_dir().join(format!("giap-vision-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(snapshot_content(path.to_str().unwrap()).await.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_caption_names_the_camera_time_and_detection() {
        let e = CameraEvent {
            id: Some(7),
            camera_id: "front-door".into(),
            event_type: "person".into(),
            confidence: Some(0.91),
            snapshot_path: Some("/tmp/x.jpg".into()),
            metadata: None,
            acknowledged: false,
            created_at: chrono::Utc::now(),
        };
        let caption = frame_caption(&e);
        assert!(caption.contains("front-door"));
        assert!(caption.contains("person"));
        assert!(caption.contains("91% confidence"));
    }

    #[test]
    fn every_camera_tool_guards_against_attached_images() {
        const NEGATIONS: &[&str] = &["not", "never", "dont", "don"];

        let router = VisionMcpServer::tool_router();
        let tools = router.list_all();
        assert!(!tools.is_empty());

        let guarded = [
            "get_recent_camera_events",
            "look_at_camera_snapshot",
            "look_at_camera_window",
            "send_camera_snapshot",
        ];
        for name in guarded {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("{name} is not registered"));
            let description = tool
                .description
                .as_deref()
                .unwrap_or_else(|| panic!("{name} has no description"));
            let lower = description.to_lowercase();
            assert!(
                lower.contains("attach"),
                "{name} must mention images attached to the message: {description}"
            );
            let guarded_clause = lower.split('.').any(|sentence| {
                sentence.contains("attach")
                    && sentence
                        .split(|c: char| !c.is_ascii_alphanumeric())
                        .any(|word| NEGATIONS.contains(&word))
            });
            assert!(
                guarded_clause,
                "{name}: mentioning attachments is not a guard - one clause must both name \
                 the attachment and rule this tool out for it: {description}"
            );
        }
    }
}
