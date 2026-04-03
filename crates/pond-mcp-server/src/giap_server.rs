use std::sync::Arc;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::{
        CallToolResult, Content, ErrorCode, ErrorData, Implementation, InitializeResult,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use crate::registry::GiapServiceHandles;

#[derive(Clone)]
pub struct GiapMcpServer {
    services: Arc<GiapServiceHandles>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl GiapMcpServer {
    pub fn new(services: Arc<GiapServiceHandles>) -> Self {
        Self {
            services,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Get the current weather conditions for the configured location.")]
    async fn get_current_weather(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match &self.services.weather {
            None => Ok(CallToolResult::success(vec![Content::text(
                "Weather is not configured on this GIAP instance.",
            )])),
            Some(w) => match w.current().await {
                Ok(data) => Ok(CallToolResult::success(vec![Content::text(
                    data.as_context_block(),
                )])),
                Err(e) => Err(ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Weather fetch error: {}", e),
                    None,
                )),
            },
        }
    }

    #[tool(description = "List all registered devices on this GIAP instance.")]
    async fn list_registered_devices(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.services.device_registry.list_devices().await {
            Ok(devices) => {
                let text = if devices.is_empty() {
                    "No devices registered.".to_string()
                } else {
                    devices
                        .iter()
                        .map(|d| {
                            format!(
                                "- {} ({}): {}",
                                d.name,
                                d.device_type,
                                if d.is_online { "online" } else { "offline" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Error listing devices: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "List all scheduled tasks on this GIAP instance.")]
    async fn list_schedules(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match &self.services.scheduler {
            None => Ok(CallToolResult::success(vec![Content::text(
                "Scheduler is not configured.",
            )])),
            Some(s) => match s.list_tasks().await {
                Ok(tasks) => {
                    let text = if tasks.is_empty() {
                        "No scheduled tasks.".to_string()
                    } else {
                        tasks
                            .iter()
                            .map(|t| {
                                format!(
                                    "- {} [{}]: {} ({})",
                                    t.label,
                                    t.id,
                                    t.cron,
                                    if t.paused { "paused" } else { "active" }
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    Ok(CallToolResult::success(vec![Content::text(text)]))
                }
                Err(e) => Err(ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Error listing schedules: {}", e),
                    None,
                )),
            },
        }
    }
}

#[tool_handler]
impl ServerHandler for GiapMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-mcp-server",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP (Goose In A Pond) MCP server. Provides tools to interact with the local smart home AI assistant.",
            )
    }
}
