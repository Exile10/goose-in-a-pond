//! Weather MCP Server — current weather conditions.
//!
//! Provides 1 tool: `get_current_weather`.
//! Depends on [`WeatherProvider`] (wrapped in `Option` for unconfigured instances).

use pond_adapters_weather::WeatherProvider;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::{
        CallToolResult, Content, ErrorCode, ErrorData, Implementation, InitializeResult,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use std::sync::Arc;

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WeatherMcpServer {
    weather: Option<Arc<dyn WeatherProvider>>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl WeatherMcpServer {
    pub fn new(weather: Option<Arc<dyn WeatherProvider>>) -> Self {
        Self {
            weather,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Get the current weather conditions for the configured location.")]
    async fn get_current_weather(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        println!(
            "[weather] get_current_weather called, provider={}",
            if self.weather.is_some() {
                "configured"
            } else {
                "NONE"
            }
        );
        match &self.weather {
            None => {
                println!("[weather] no provider configured, returning guidance");
                Ok(CallToolResult::success(vec![Content::text(
                    "The weather service is not configured on this GIAP instance. \
                     Inform the user that they need to configure a weather location in their settings. \
                     DO NOT attempt to fetch weather using any other tool, shell command, or external request.",
                )]))
            }
            Some(w) => match w.current().await {
                Ok(data) => {
                    println!("[weather] fetched successfully");
                    Ok(CallToolResult::success(vec![Content::text(
                        data.as_context_block(),
                    )]))
                }
                Err(e) => {
                    println!("[weather] fetch FAILED: {e}");
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Weather fetch failed: {e}. Tell the user the weather service \
                         is temporarily unavailable and suggest they try again shortly."
                    ))]))
                }
            },
        }
    }
}

#[tool_handler]
impl ServerHandler for WeatherMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-weather",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Weather MCP server — current weather conditions for the configured location.\n\n\
                 Tool: get_current_weather. Returns temperature, humidity, conditions, and wind \
                 for the user's configured location. If not configured, instructs the user to \
                 set up a weather location in settings.",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct WeatherDeps {
    weather: Option<Arc<dyn WeatherProvider>>,
}

static WEATHER_DEPS: OnceLock<WeatherDeps> = OnceLock::new();

/// Initialize weather server dependencies. Call once at startup.
pub fn init_weather_deps(weather: Option<Arc<dyn WeatherProvider>>) {
    let _ = WEATHER_DEPS.set(WeatherDeps { weather });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_weather_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = WEATHER_DEPS.get().expect("init_weather_deps() not called");
    let server = WeatherMcpServer::new(deps.weather.clone());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-weather MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_constructs_with_none() {
        let _server = WeatherMcpServer::new(None);
    }
}
