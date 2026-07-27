//! Vision MCP Server (#130, Q2-53) — expose recent camera/vision events.
//!
//! Read-mostly tools over the `camera_events` store so the agent can answer
//! "what did the camera see?" grounded in on-device detections (from the
//! vision pipeline in `pond-adapters-vision`) and externally posted events
//! alike. Acknowledging an event suppresses re-alerts.
//!
//! Provides 2 tools: `get_recent_camera_events`, `acknowledge_camera_event`.

use std::sync::{Arc, OnceLock};

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
List recent camera/vision events (motion, pet, package, person), newest first. Never guess \
what a camera saw.")]
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
                format!("No recent events from camera '{camera_id}'."),
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
                "GIAP Vision MCP server — recent camera/vision events from the local, \
                 on-device event store (never guess what a camera saw).\n\n\
                 Tools: get_recent_camera_events (what the camera detected, newest first), \
                 acknowledge_camera_event (dismiss an alert by ID).",
            )
    }
}

// ── Static deps + spawn function for the Goose builtin registry ───────────────

use rmcp::ServiceExt;
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
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-vision MCP server failed: {e}"),
        }
    });
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
}
