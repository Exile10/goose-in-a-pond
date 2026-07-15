//! Sensor Data Aggregator MCP Server — query stored IoT sensor readings.
//!
//! Provides 3 tools: `get_sensor_reading`, `get_sensor_history`, `list_sensors`.

use std::sync::{Arc, OnceLock};

use pond_core::user_data::ports::sensor_storage::SensorStorage;
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

// ── Parameter structs ──────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetSensorReadingParams {
    /// The device ID (e.g. "bedroom", "kitchen", "sensor-01").
    pub device_id: Option<String>,
    /// The sensor type (e.g. "temperature", "humidity", "pressure").
    pub sensor_type: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetSensorHistoryParams {
    /// The device ID (e.g. "bedroom").
    pub device_id: Option<String>,
    /// The sensor type (e.g. "temperature").
    pub sensor_type: Option<String>,
    /// ISO 8601 start time, inclusive (e.g. "2024-01-01T00:00:00Z"). Omit for all history.
    pub since: Option<String>,
    /// ISO 8601 end time, exclusive. Omit for up to now.
    pub until: Option<String>,
    /// Aggregation: "min", "max", "avg", or omit for raw readings.
    pub agg: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSensorsParams {
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── MCP server ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SensorsMcpServer {
    sensor_storage: Arc<dyn SensorStorage + Send + Sync>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl SensorsMcpServer {
    pub fn new(sensor_storage: Arc<dyn SensorStorage + Send + Sync>) -> Self {
        Self {
            sensor_storage,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Get the latest sensor reading for a device. Use for questions like \
'What is the temperature in the bedroom?' or 'What is the humidity in the kitchen?'. \
Pass device_id (the room/device name) and sensor_type ('temperature', 'humidity', etc.).")]
    async fn get_sensor_reading(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<GetSensorReadingParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_sensor_reading");

        let device_id = resolve_str_param(
            &params.0.device_id,
            &params.0.extra,
            &["device_id", "device", "room", "location"],
        );
        let sensor_type = resolve_str_param(
            &params.0.sensor_type,
            &params.0.extra,
            &["sensor_type", "type", "sensor"],
        );

        let (Some(device_id), Some(sensor_type)) = (device_id, sensor_type) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "Please provide both `device_id` and `sensor_type` (e.g. device_id='bedroom', sensor_type='temperature').",
            )]));
        };

        match self
            .sensor_storage
            .get_latest(&device_id, &sensor_type)
            .await
        {
            Ok(Some(r)) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Latest {} reading from '{}': {} {} (recorded at {})",
                r.sensor_type,
                r.device_id,
                r.value,
                r.unit,
                r.recorded_at.format("%Y-%m-%d %H:%M:%S UTC"),
            ))])),
            Ok(None) => Ok(CallToolResult::success(vec![Content::text(format!(
                "No '{}' readings found for device '{}'. The sensor may not have reported yet.",
                sensor_type, device_id,
            ))])),
            Err(e) => {
                tracing::warn!(error = %e, device_id, sensor_type, "sensors: get_latest failed");
                Ok(CallToolResult::success(vec![Content::text(
                    "Failed to retrieve the sensor reading. Please try again.",
                )]))
            }
        }
    }

    #[tool(description = "\
Get sensor reading history for a device within an optional time range. \
Supports aggregation: pass agg='min', 'max', or 'avg' for a single aggregate value, \
or omit for the raw time series. Use for 'What was the temperature trend today?' or \
'What was the peak humidity this week?'.")]
    async fn get_sensor_history(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<GetSensorHistoryParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_sensor_history");

        let device_id = resolve_str_param(
            &params.0.device_id,
            &params.0.extra,
            &["device_id", "device", "room"],
        );
        let sensor_type = resolve_str_param(
            &params.0.sensor_type,
            &params.0.extra,
            &["sensor_type", "type", "sensor"],
        );

        let (Some(device_id), Some(sensor_type)) = (device_id, sensor_type) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "Please provide both `device_id` and `sensor_type`.",
            )]));
        };

        let since = params.0.since.as_deref().and_then(parse_datetime);
        let until = params.0.until.as_deref().and_then(parse_datetime);
        let agg = params.0.agg.as_deref().map(str::to_lowercase);

        let readings = match self
            .sensor_storage
            .get_history(&device_id, &sensor_type, since, until)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, device_id, sensor_type, "sensors: get_history failed");
                return Ok(CallToolResult::success(vec![Content::text(
                    "Failed to retrieve sensor history. Please try again.",
                )]));
            }
        };

        if readings.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No '{}' history found for device '{}'{}.",
                sensor_type,
                device_id,
                if since.is_some() || until.is_some() {
                    " in the requested time range"
                } else {
                    ""
                },
            ))]));
        }

        let unit = &readings[0].unit;
        let values: Vec<f64> = readings.iter().map(|r| r.value).collect();

        let text = match agg.as_deref() {
            Some("min") => {
                let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
                format!("Min {} for '{}': {} {}", sensor_type, device_id, min, unit)
            }
            Some("max") => {
                let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                format!("Max {} for '{}': {} {}", sensor_type, device_id, max, unit)
            }
            Some("avg") => {
                let avg = values.iter().sum::<f64>() / values.len() as f64;
                format!(
                    "Average {} for '{}': {:.2} {} (over {} readings)",
                    sensor_type,
                    device_id,
                    avg,
                    unit,
                    values.len()
                )
            }
            _ => {
                let lines: Vec<String> = readings
                    .iter()
                    .take(50)
                    .map(|r| {
                        format!(
                            "  {} — {} {}",
                            r.recorded_at.format("%Y-%m-%d %H:%M"),
                            r.value,
                            r.unit
                        )
                    })
                    .collect();
                format!(
                    "{} history for '{}' ({} readings):\n{}",
                    sensor_type,
                    device_id,
                    readings.len(),
                    lines.join("\n"),
                )
            }
        };

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "\
List all sensors (device + type pairs) that have stored readings. Use to discover \
what sensor data is available (e.g. 'what sensors do you have?', 'which rooms have temperature sensors?').")]
    async fn list_sensors(
        &self,
        _ctx: RequestContext<RoleServer>,
        _params: Parameters<ListSensorsParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("list_sensors");

        match self.sensor_storage.list_sensors().await {
            Ok(pairs) if pairs.is_empty() => Ok(CallToolResult::success(vec![Content::text(
                "No sensor readings have been recorded yet.",
            )])),
            Ok(pairs) => {
                let lines: Vec<String> = pairs
                    .iter()
                    .map(|(device, stype)| format!("  {device} — {stype}"))
                    .collect();
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Known sensors ({} total):\n{}",
                    pairs.len(),
                    lines.join("\n"),
                ))]))
            }
            Err(e) => {
                tracing::warn!(error = %e, "sensors: list_sensors failed");
                Ok(CallToolResult::success(vec![Content::text(
                    "Failed to list sensors. Please try again.",
                )]))
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for SensorsMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-sensors",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Sensor MCP server — query stored IoT sensor readings.\n\n\
                 Tools:\n\
                 - get_sensor_reading: latest value for a device + sensor type\n\
                 - get_sensor_history: time-range history with optional min/max/avg aggregation\n\
                 - list_sensors: discover all device + sensor type pairs with stored data\n\n\
                 Never guess sensor values — always use these tools to retrieve stored readings.",
            )
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn resolve_str_param(
    direct: &Option<String>,
    extra: &std::collections::HashMap<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    if let Some(s) = direct.as_deref().filter(|s| !s.trim().is_empty()) {
        return Some(s.trim().to_string());
    }
    for key in keys {
        if let Some(val) = extra.get(*key).and_then(|v| v.as_str()) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn parse_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    s.parse::<chrono::DateTime<chrono::Utc>>().ok().or_else(|| {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
            .ok()
            .map(|ndt| ndt.and_utc())
    })
}

// ── Static deps + spawn function for the Goose builtin registry ───────────────

use rmcp::ServiceExt;
use tokio::io::DuplexStream;

struct SensorDeps {
    sensor_storage: Arc<dyn SensorStorage + Send + Sync>,
}

static SENSOR_DEPS: OnceLock<SensorDeps> = OnceLock::new();

/// Install the sensor server's storage handle. Call once at startup,
/// before any chat session loads the extension.
pub fn init_sensor_deps(sensor_storage: Arc<dyn SensorStorage + Send + Sync>) {
    let _ = SENSOR_DEPS.set(SensorDeps { sensor_storage });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_sensor_server(reader: DuplexStream, writer: DuplexStream) {
    let Some(deps) = SENSOR_DEPS.get() else {
        tracing::error!(
            "spawn_sensor_server called before init_sensor_deps — sensor MCP server will not start"
        );
        return;
    };
    let server = SensorsMcpServer::new(deps.sensor_storage.clone());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-sensors MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use pond_core::user_data::domain::sensor::SensorReading;
    use pond_core::user_data::ports::sensor_storage::SensorStorage;
    use rmcp::model::RequestId;
    use rmcp::service::serve_directly;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    // Minimal in-test stub — avoids the `pond-core/test-mocks` feature gate.
    struct StubStorage {
        readings: Arc<RwLock<Vec<SensorReading>>>,
    }

    impl StubStorage {
        fn new() -> Self {
            Self {
                readings: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl SensorStorage for StubStorage {
        async fn record(&self, reading: SensorReading) -> Result<()> {
            self.readings.write().await.push(reading);
            Ok(())
        }

        async fn get_latest(
            &self,
            device_id: &str,
            sensor_type: &str,
        ) -> Result<Option<SensorReading>> {
            let r = self.readings.read().await;
            Ok(r.iter()
                .filter(|r| r.device_id == device_id && r.sensor_type == sensor_type)
                .max_by_key(|r| r.recorded_at)
                .cloned())
        }

        async fn get_recent(&self, device_id: &str, limit: usize) -> Result<Vec<SensorReading>> {
            let r = self.readings.read().await;
            let mut v: Vec<_> = r
                .iter()
                .filter(|r| r.device_id == device_id)
                .cloned()
                .collect();
            v.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
            v.truncate(limit);
            Ok(v)
        }

        async fn get_history(
            &self,
            device_id: &str,
            sensor_type: &str,
            since: Option<DateTime<Utc>>,
            until: Option<DateTime<Utc>>,
        ) -> Result<Vec<SensorReading>> {
            let r = self.readings.read().await;
            let mut v: Vec<_> = r
                .iter()
                .filter(|r| {
                    r.device_id == device_id
                        && r.sensor_type == sensor_type
                        && since.map_or(true, |s| r.recorded_at >= s)
                        && until.map_or(true, |u| r.recorded_at < u)
                })
                .cloned()
                .collect();
            v.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
            Ok(v)
        }

        async fn list_sensors(&self) -> Result<Vec<(String, String)>> {
            let r = self.readings.read().await;
            let mut seen = std::collections::HashSet::new();
            let mut result = Vec::new();
            for reading in r.iter() {
                let key = (reading.device_id.clone(), reading.sensor_type.clone());
                if seen.insert(key.clone()) {
                    result.push(key);
                }
            }
            result.sort();
            Ok(result)
        }
    }

    fn make_ctx() -> RequestContext<RoleServer> {
        let (_client, stream) = tokio::io::duplex(64);
        let running = serve_directly(
            SensorsMcpServer::new(Arc::new(StubStorage::new())),
            stream,
            None,
        );
        RequestContext::new(RequestId::Number(0), running.peer().clone())
    }

    fn reading(device_id: &str, sensor_type: &str, value: f64) -> SensorReading {
        SensorReading {
            device_id: device_id.to_string(),
            sensor_type: sensor_type.to_string(),
            value,
            unit: "C".to_string(),
            recorded_at: chrono::Utc::now(),
        }
    }

    fn text_of(result: CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text())
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join("")
    }

    #[tokio::test]
    async fn get_sensor_reading_returns_latest() {
        let storage = Arc::new(StubStorage::new());
        storage
            .record(reading("bedroom", "temperature", 21.0))
            .await
            .unwrap();
        storage
            .record(reading("bedroom", "temperature", 23.5))
            .await
            .unwrap();

        let server = SensorsMcpServer::new(storage);
        let params = Parameters(GetSensorReadingParams {
            device_id: Some("bedroom".to_string()),
            sensor_type: Some("temperature".to_string()),
            extra: Default::default(),
        });
        let text = text_of(server.get_sensor_reading(make_ctx(), params).await.unwrap());
        assert!(
            text.contains("23.5"),
            "expected latest value 23.5 in: {text}"
        );
        assert!(text.contains("bedroom"), "expected device_id in: {text}");
    }

    #[tokio::test]
    async fn get_sensor_reading_missing_params_returns_guidance() {
        let server = SensorsMcpServer::new(Arc::new(StubStorage::new()));
        let params = Parameters(GetSensorReadingParams::default());
        let text = text_of(server.get_sensor_reading(make_ctx(), params).await.unwrap());
        assert!(
            text.contains("device_id"),
            "should prompt for device_id: {text}"
        );
    }

    #[tokio::test]
    async fn list_sensors_shows_known_devices() {
        let storage = Arc::new(StubStorage::new());
        storage
            .record(reading("bedroom", "temperature", 21.0))
            .await
            .unwrap();
        storage
            .record(reading("kitchen", "humidity", 55.0))
            .await
            .unwrap();

        let server = SensorsMcpServer::new(storage);
        let text = text_of(
            server
                .list_sensors(make_ctx(), Parameters(ListSensorsParams::default()))
                .await
                .unwrap(),
        );
        assert!(text.contains("bedroom"), "should list bedroom: {text}");
        assert!(text.contains("kitchen"), "should list kitchen: {text}");
    }

    #[tokio::test]
    async fn get_sensor_history_avg_aggregation() {
        let storage = Arc::new(StubStorage::new());
        storage
            .record(reading("living-room", "temperature", 20.0))
            .await
            .unwrap();
        storage
            .record(reading("living-room", "temperature", 24.0))
            .await
            .unwrap();
        storage
            .record(reading("living-room", "temperature", 22.0))
            .await
            .unwrap();

        let server = SensorsMcpServer::new(storage);
        let params = Parameters(GetSensorHistoryParams {
            device_id: Some("living-room".to_string()),
            sensor_type: Some("temperature".to_string()),
            since: None,
            until: None,
            agg: Some("avg".to_string()),
            extra: Default::default(),
        });
        let text = text_of(server.get_sensor_history(make_ctx(), params).await.unwrap());
        assert!(text.contains("22.00"), "expected avg 22.00 in: {text}");
    }

    #[test]
    fn server_constructs() {
        let _server = SensorsMcpServer::new(Arc::new(StubStorage::new()));
    }

    #[test]
    fn parse_datetime_rfc3339() {
        assert!(parse_datetime("2024-01-15T10:00:00Z").is_some());
        assert!(parse_datetime("not-a-date").is_none());
    }
}
