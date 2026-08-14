//! Weather MCP Server — current weather and forecasts.
//!
//! Provides 2 tools: `get_current_weather`, `get_weather_forecast`.
//! Depends on [`WeatherProvider`] (wrapped in `Option` for unconfigured instances).

use pond_adapters_weather::WeatherProvider;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ─────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WeatherParams {
    pub location: Option<String>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ForecastParams {
    pub location: Option<String>,
    /// 1-7, default 3.
    pub days: Option<u8>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

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

    #[tool(description = "\
Get current weather. Omit location to use the user's configured home — call it \
that way rather than asking which city. Never guess weather data or use shell \
commands for it.")]
    async fn get_current_weather(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WeatherParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_current_weather");
        let location = resolve_location(&params.0);
        tracing::debug!(
            "get_current_weather called, location={:?}, provider={}",
            location,
            if self.weather.is_some() {
                "configured"
            } else {
                "NONE"
            }
        );

        match &self.weather {
            None => Ok(CallToolResult::success(vec![Content::text(
                "The weather service is not configured on this GIAP instance. \
                 Inform the user that they need to configure a weather location in their settings. \
                 DO NOT attempt to fetch weather using any other tool, shell command, or external request.",
            )])),
            Some(w) => {
                let result = match &location {
                    Some(loc) => w.current_for(loc).await,
                    None => w.current().await,
                };
                match result {
                    Ok(data) => {
                        tracing::debug!("weather: fetched current for {}", data.location_name);
                        let ui_data = serde_json::json!({
                            "location": data.location_name,
                            "temperature": data.temperature_c,
                            "feels_like": data.feels_like_c,
                            "condition": data.description,
                            "humidity": data.humidity_pct,
                            "wind_speed": data.wind_speed_kmh,
                            "wind_gusts": data.wind_gusts_kmh,
                            "cloud_cover": data.cloud_cover_pct,
                            "precipitation": data.precipitation_mm,
                            "is_day": data.is_day,
                            "sunrise": data.sunrise,
                            "sunset": data.sunset,
                        });
                        let hint = format!("[[[mcp-ui:weather:{}]]]\n", ui_data);
                        let full_result = format!("{}{}", hint, data.as_context_block());
                        Ok(CallToolResult::success(vec![Content::text(full_result)]))
                    }
                    Err(e) => {
                        tracing::warn!("weather: fetch failed: {e}");
                        Ok(CallToolResult::success(vec![Content::text(format!(
                            "Weather fetch failed: {e}. Tell the user the weather service \
                             is temporarily unavailable and suggest they try again shortly."
                        ))]))
                    }
                }
            }
        }
    }

    #[tool(description = "\
Multi-day forecast: highs/lows, rain chance, UV, sunrise/sunset. \
days 1-7 (default 3). Omit location to use the user's configured home rather \
than asking which city. Never guess data.")]
    async fn get_weather_forecast(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ForecastParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_weather_forecast");
        let location = resolve_forecast_location(&params.0);
        let days = params.0.days.unwrap_or(3).clamp(1, 7);

        tracing::debug!(
            "get_weather_forecast called, location={:?}, days={}, provider={}",
            location,
            days,
            if self.weather.is_some() {
                "configured"
            } else {
                "NONE"
            }
        );

        match &self.weather {
            None => Ok(CallToolResult::success(vec![Content::text(
                "The weather service is not configured on this GIAP instance. \
                 Inform the user that they need to configure a weather location in their settings. \
                 DO NOT attempt to fetch forecasts using any other tool, shell command, or external request.",
            )])),
            Some(w) => {
                let result = match &location {
                    Some(loc) => w.forecast_for(loc, days).await,
                    None => w.forecast(days).await,
                };
                match result {
                    Ok(data) => {
                        tracing::debug!(
                            "weather: fetched {}-day forecast for {}",
                            data.days.len(),
                            data.location_name
                        );
                        // Same `[[[mcp-ui:…]]]` marker the current-weather tool
                        // emits. Without it `extract_ui_hint` returns no
                        // `renderHint`, and the desktop deliberately refuses to
                        // render a card it has no structured data for — which is
                        // why a forecast used to arrive as a wall of text next to
                        // a proper weather card.
                        let ui_data = serde_json::json!({
                            "location": data.location_name,
                            "forecast": data.days.iter().map(|d| serde_json::json!({
                                "date": d.date,
                                "description": d.description,
                                "temp_max_c": d.temp_max_c,
                                "temp_min_c": d.temp_min_c,
                                "precipitation_sum_mm": d.precipitation_sum_mm,
                                "precipitation_probability_pct": d.precipitation_probability_pct,
                                "wind_speed_max_kmh": d.wind_speed_max_kmh,
                                "uv_index_max": d.uv_index_max,
                                "sunrise": d.sunrise,
                                "sunset": d.sunset,
                            })).collect::<Vec<_>>(),
                        });
                        let hint = format!("[[[mcp-ui:weather:{}]]]\n", ui_data);
                        let full_result = format!("{}{}", hint, data.as_context_block());
                        Ok(CallToolResult::success(vec![Content::text(full_result)]))
                    }
                    Err(e) => {
                        tracing::warn!("weather: forecast fetch failed: {e}");
                        Ok(CallToolResult::success(vec![Content::text(format!(
                            "Forecast fetch failed: {e}. Tell the user the weather service \
                             is temporarily unavailable and suggest they try again shortly."
                        ))]))
                    }
                }
            }
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
                "GIAP Weather MCP server — current conditions and multi-day forecasts.\n\n\
                 Tools:\n\
                 - get_current_weather: temperature, humidity, wind, cloud cover, sunrise/sunset. \
                 Pass 'location' for any city or omit for the user's default.\n\
                 - get_weather_forecast: multi-day forecast with highs/lows, rain chance, UV index. \
                 Pass 'location' and 'days' (1-7).\n\n\
                 If weather is not configured, instructs the user to set up a location in settings.",
            )
    }
}

// ── Param resolution ──────────────────────────────────────────────────────────

/// Extract location from WeatherParams, scanning extras as fallback.
fn resolve_location(params: &WeatherParams) -> Option<String> {
    // Direct param
    if let Some(ref loc) = params.location {
        let trimmed = loc.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    // Scan extras for common synonyms
    for key in &["location", "city", "place", "loc", "where"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

/// Extract location from ForecastParams, scanning extras as fallback.
fn resolve_forecast_location(params: &ForecastParams) -> Option<String> {
    if let Some(ref loc) = params.location {
        let trimmed = loc.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for key in &["location", "city", "place", "loc", "where"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

// ── MCP App resource ─────────────────────────────────────────────────────

/// Self-contained HTML weather card (MCP App).
/// Embedded at compile time — no filesystem access required at runtime.
const WEATHER_APP_HTML: &str = include_str!("../apps/weather-card.html");

/// Resource URI for the weather MCP App.
pub const WEATHER_APP_URI: &str = "ui://giap-weather/weather-card.html";

/// Returns all `(uri, html_content)` pairs for resources served by this MCP server.
pub fn app_resources() -> Vec<(&'static str, &'static str)> {
    vec![(WEATHER_APP_URI, WEATHER_APP_HTML)]
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

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
    crate::serve_builtin("giap-weather", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_constructs_with_none() {
        let _server = WeatherMcpServer::new(None);
    }

    #[test]
    fn resolve_location_from_direct_param() {
        let params = WeatherParams {
            location: Some("Kisumu".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_location(&params), Some("Kisumu".to_string()));
    }

    #[test]
    fn resolve_location_from_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "city".to_string(),
            serde_json::Value::String("Mombasa".to_string()),
        );
        let params = WeatherParams {
            location: None,
            extra,
        };
        assert_eq!(resolve_location(&params), Some("Mombasa".to_string()));
    }

    #[test]
    fn resolve_location_returns_none_for_empty() {
        let params = WeatherParams {
            location: Some("  ".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_location(&params), None);
    }

    #[test]
    fn resolve_location_returns_none_for_no_params() {
        let params = WeatherParams::default();
        assert_eq!(resolve_location(&params), None);
    }

    #[test]
    fn resolve_forecast_location_works() {
        let params = ForecastParams {
            location: Some("Eldoret".to_string()),
            days: Some(5),
            extra: Default::default(),
        };
        assert_eq!(
            resolve_forecast_location(&params),
            Some("Eldoret".to_string())
        );
    }
}
