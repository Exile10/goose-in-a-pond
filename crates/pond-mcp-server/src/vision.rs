//! Vision MCP Server (#130, Q2-53) — expose recent camera/vision events.
//!
//! Read-mostly tools over the `camera_events` store so the agent can answer
//! "what did the camera see?" grounded in on-device detections (from the
//! vision pipeline in `pond-adapters-vision`) and externally posted events
//! alike. Acknowledging an event suppresses re-alerts.
//!
//! Provides 4 tools: `get_recent_camera_events`, `acknowledge_camera_event`,
//! `look_at_camera_snapshot` (phase F3), `look_at_camera_window` (phase F5).
//!
//! # Frames, not prose (phases F3 and F5)
//!
//! The two listing tools describe what the on-device detector CLASSIFIED. That
//! is all a text-only pipeline could offer, and it is not enough for "what is at
//! the door?" — the answer to that lives in the pixels, which have been sitting
//! on disk as JPEGs (`camera_events.snapshot_path`, written by
//! `pond-adapters-vision`) with no way to reach the model.
//!
//! The two `look_at_*` tools return those JPEGs as MCP image content. On GIAP's
//! local engine the image is then lifted into a top-level message part by
//! `GiapProviderShim::promote_tool_result_images` — see that function for why
//! the engine cannot read a tool-nested image on its own.

use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use pond_core::user_data::domain::sensor::CameraEvent;
use pond_core::user_data::ports::camera_storage::CameraStorage;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, Implementation, InitializeResult, ProtocolVersion,
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

/// How many recent events a window samples FROM. Bounds the DB read; the frame
/// cap bounds what the model actually sees.
const WINDOW_EVENT_SCAN: usize = 30;

// ── Parameter structs ──────────────────────────────────────────────────────--

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

/// Read a snapshot from disk and turn it into MCP image content.
///
/// Returns `None` (with a log) for anything unreadable, oversized, or not an
/// image, so a stale row can never fail a tool call — the caller degrades to a
/// text answer, which is what the tool did before these existed.
async fn snapshot_content(path: &str) -> Option<Content> {
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

    // Check the size before reading: an operator-swapped file could be anything,
    // and the vision encoder budget is the same one manual attachments obey.
    match tokio::fs::metadata(p).await {
        Ok(m) if m.len() as usize > MAX_IMAGE_BYTES => {
            tracing::warn!(
                path,
                bytes = m.len(),
                "vision: snapshot exceeds the per-image limit; not sending it to the model"
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
        Ok(bytes) => Some(Content::image(
            base64::engine::general_purpose::STANDARD.encode(&bytes),
            mime.to_string(),
        )),
        Err(e) => {
            tracing::warn!(path, error = %e, "vision: could not read snapshot");
            None
        }
    }
}

/// One-line description of an event, used as the text that accompanies a frame
/// so the model knows when it was taken and what the detector thought it was.
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

// ── MCP server ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct VisionMcpServer {
    camera_storage: Arc<dyn CameraStorage + Send + Sync>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
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
            Ok(events) if events.is_empty() => Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results(
                    &format!("recent events from camera '{camera_id}'"),
                    &["giap-vision__look_at_camera_snapshot"],
                ),
            )])),
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
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Recent events from camera '{camera_id}':\n{}",
                    lines.join("\n"),
                ))]))
            }
            Err(e) => {
                tracing::warn!(error = %e, "vision: camera event listing failed");
                Ok(CallToolResult::success(vec![Content::text(
                    "Sorry, I couldn't read the camera events right now.",
                )]))
            }
        }
    }

    // The "not for attached images" clause is not padding. Measured on
    // gemma-4-E2B: with a description that merely said "use this when the user
    // asks what something looks like", a turn with an image ATTACHED to the
    // message made the model call this tool instead of looking at the image it
    // had already been given, get "No frame available", and answer "I cannot see
    // the image". The model needs the boundary spelled out.
    //
    // It is spelled out FIRST, before the capability. With the guard trailing,
    // a 4B model still answered "what do you see?" by offering camera frames
    // while an attachment sat in its context: the opening words decided the
    // match and the qualifier arrived too late. Scope, then guard, then what it
    // returns.
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

        let events = match self
            .camera_storage
            .list_events(&camera_id, WINDOW_EVENT_SCAN)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "vision: camera event listing failed");
                return Ok(CallToolResult::success(vec![Content::text(
                    "Sorry, I couldn't reach the camera store right now.",
                )]));
            }
        };

        // `list_events` is newest-first, so the first match with a frame is the
        // newest frame when no specific event was named.
        let chosen = match params.0.event_id {
            Some(id) => events.iter().find(|e| e.id == Some(id)),
            None => events.iter().find(|e| e.snapshot_path.is_some()),
        };

        let Some(event) = chosen else {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No frame available from camera '{camera_id}'."
            ))]));
        };
        let Some(path) = event.snapshot_path.as_deref() else {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Event {} has no saved frame.",
                event.id.unwrap_or(-1)
            ))]));
        };
        let Some(image) = snapshot_content(path).await else {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "The saved frame for event {} is no longer readable.",
                event.id.unwrap_or(-1)
            ))]));
        };

        Ok(CallToolResult::success(vec![
            Content::text(frame_caption(event)),
            image,
        ]))
    }

    #[tool(description = "\
Acknowledge a camera event by ID to stop re-alerts once the user has seen it.")]
    async fn acknowledge_camera_event(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<AcknowledgeCameraEventParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let Some(event_id) = params.0.event_id else {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need an `event_id` (see get_recent_camera_events).",
            )]));
        };
        match self.camera_storage.acknowledge(event_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Camera event {event_id} acknowledged."
            ))])),
            Err(e) => {
                tracing::warn!(error = %e, event_id, "vision: acknowledge failed");
                Ok(CallToolResult::success(vec![Content::text(format!(
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
                 Tools: get_recent_camera_events (what the detector classified, newest first), \
                 look_at_camera_snapshot (one camera frame — \"what does the driveway look \
                 like\"), look_at_camera_window (several frames across time, to see what \
                 changed), acknowledge_camera_event (dismiss an alert by ID).",
            )
    }
}

// ── Static deps + spawn function for the Goose builtin registry ───────────────

use tokio::io::DuplexStream;

struct VisionDeps {
    camera_storage: Arc<dyn CameraStorage + Send + Sync>,
}

static VISION_DEPS: OnceLock<VisionDeps> = OnceLock::new();

/// Install the vision server's camera-event store handle. Call once at
/// startup, before any chat session loads the extension.
pub fn init_vision_deps(camera_storage: Arc<dyn CameraStorage + Send + Sync>) {
    let _ = VISION_DEPS.set(VisionDeps { camera_storage });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_vision_server(reader: DuplexStream, writer: DuplexStream) {
    // Degrade gracefully instead of panicking: if an entry point registered the
    // giap-vision extension without calling `init_vision_deps` first, we simply
    // do not start the server — Goose sees the pipe close and treats the
    // extension as unavailable for the session.
    let Some(deps) = VISION_DEPS.get() else {
        tracing::error!(
            "giap-vision: init_vision_deps() was never called for this entry point; \
             vision MCP server not started (camera tools unavailable this session)"
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

    // ── F3: snapshot -> MCP image content ────────────────────────────────

    #[tokio::test]
    async fn a_real_jpeg_becomes_image_content() {
        let dir = std::env::temp_dir().join(format!("giap-vision-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("camera-1-20260728T000000000Z.jpg");
        std::fs::write(&path, b"\xFF\xD8\xFF\xE0 pretend jpeg").unwrap();

        let content = snapshot_content(path.to_str().unwrap())
            .await
            .expect("readable jpeg");
        match content.raw {
            rmcp::model::RawContent::Image(img) => {
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

    /// A stale row must degrade to "no frame", never fail the call.
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

    #[tokio::test]
    async fn an_oversized_snapshot_is_refused_rather_than_sent() {
        use pond_core::models::domain::image_limits::MAX_IMAGE_BYTES;
        let dir = std::env::temp_dir().join(format!("giap-vision-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("huge.jpg");
        std::fs::write(&path, vec![0u8; MAX_IMAGE_BYTES + 1024]).unwrap();
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

    // ── The attached-image / camera-frame boundary ───────────────────────

    /// Every camera tool description must still mention message attachments in
    /// the same sentence as a negation — the shape a guard clause has.
    ///
    /// # What this does and does not prove
    ///
    /// This is a STRUCTURAL check, not a semantic one. It catches the failure
    /// worth catching: someone reworks a description and the guard silently
    /// disappears. It cannot tell a real guard from a sentence that happens to
    /// contain both signals — "Attached to the roof, this camera never stops
    /// recording." passes it with no guard at all. Do not read a pass as
    /// confirmation that the wording guards anything; read a failure as proof
    /// that it does not.
    ///
    /// It replaced a positional check ("attached" must appear in the first half
    /// of the string), which failed a reworded guard for no reason. Placement
    /// still matters — a 4B model matched on the opening clause and called a
    /// camera tool with an image already in context, which is why the guard
    /// leads each description — but placement is a wording decision no assertion
    /// here pins down.
    #[test]
    fn every_camera_tool_guards_against_attached_images() {
        /// The negations that turn a mention of attachments into a guard.
        /// Matched as whole words so "another" cannot pass for "not".
        const NEGATIONS: &[&str] = &["not", "never", "dont", "don"];

        let router = VisionMcpServer::tool_router();
        let tools = router.list_all();
        assert!(!tools.is_empty());

        // acknowledge_camera_event takes an ID and returns no frames — it cannot
        // be confused for looking at an attachment.
        let guarded = ["get_recent_camera_events", "look_at_camera_snapshot"];
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
            // The guard has to be one clause: a sentence that mentions the
            // attachment AND negates acting on it. Split on '.' so a negation
            // three sentences away cannot stand in for a real guard.
            let guarded_clause = lower.split('.').any(|sentence| {
                sentence.contains("attach")
                    && sentence
                        .split(|c: char| !c.is_ascii_alphabetic())
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
