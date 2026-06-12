//! Device Control MCP Server — actuation tools for smart home devices.
//!
//! Provides 3 tools: `list_controllable_devices`, `set_device_state`, `get_device_state`.
//! Depends on [`DeviceRegistry`] (for device lookup) and [`DeviceController`] (for actuation).
//!
//! Errors are returned as `CallToolResult::success(guidance)` so the LLM can self-correct.
//! Sensitive devices (locks/doors/alarms) require `confirmed: true` before actuation.

use pond_core::domain::device::StateValue;
use pond_core::ports::device_controller::DeviceController;
use pond_core::ports::device_registry::{Device, DeviceRegistry};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorCode, ErrorData, Implementation, InitializeResult,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ──────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SetDeviceStateParams {
    /// Device ID returned by list_controllable_devices.
    pub device_id: Option<String>,
    /// State key: "power" for on/off, or a capability key like "brightness", "mode".
    pub state_key: Option<String>,
    /// Value as string: "on"/"off" for power; number string for ranges; text for modes.
    pub value: Option<String>,
    /// Required true for locks, doors, and alarms before the command is sent.
    pub confirmed: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetDeviceStateParams {
    /// Device ID returned by list_controllable_devices.
    pub device_id: Option<String>,
}

// ── MCP server ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ControlMcpServer {
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    device_controller: Option<Arc<dyn DeviceController + Send + Sync>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ControlMcpServer {
    pub fn new(
        device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
        device_controller: Option<Arc<dyn DeviceController + Send + Sync>>,
    ) -> Self {
        Self {
            device_registry,
            device_controller,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "List devices that can be controlled (have a transport address set).")]
    async fn list_controllable_devices(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let devices = self.device_registry.list_devices().await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
        })?;

        let controllable: Vec<&Device> = devices
            .iter()
            .filter(|d| d.address.is_some())
            .collect();

        if controllable.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No controllable devices found. Register a device with transport='mqtt' and an address to enable control.",
            )]));
        }

        let controller_available = self.device_controller.is_some();
        let mut lines = Vec::new();

        for d in &controllable {
            let caps: Vec<String> = d
                .structured_capabilities
                .iter()
                .map(|c| c.key.clone())
                .collect();
            let caps_str = if caps.is_empty() {
                "power".to_string()
            } else {
                caps.join(", ")
            };
            lines.push(format!(
                "- id={} name=\"{}\" transport={} status={} capabilities=[{}]",
                d.id,
                d.name,
                d.transport,
                if d.is_online { "online" } else { "offline" },
                caps_str,
            ));
        }

        let mut text = lines.join("\n");
        if !controller_available {
            text.push_str(
                "\n\nNote: device controller not active (set MQTT_HOST to enable actuation).",
            );
        }
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Set a device state (power on/off or any capability key). Locks/doors require confirmed=true."
    )]
    async fn set_device_state(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SetDeviceStateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;

        // ── Validate required fields ──────────────────────────────────────────
        let device_id = match &p.device_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => return Ok(CallToolResult::success(vec![Content::text(
                "Please provide device_id. Use list_controllable_devices to find the correct ID.",
            )])),
        };
        let state_key = match &p.state_key {
            Some(k) if !k.is_empty() => k.clone(),
            _ => return Ok(CallToolResult::success(vec![Content::text(
                "Please provide state_key (e.g. \"power\", \"brightness\", \"mode\").",
            )])),
        };
        let value = match &p.value {
            Some(v) if !v.is_empty() => v.clone(),
            _ => return Ok(CallToolResult::success(vec![Content::text(
                "Please provide value (e.g. \"on\", \"off\", \"200\", \"heat\").",
            )])),
        };

        // ── Controller availability ───────────────────────────────────────────
        let ctrl = match &self.device_controller {
            Some(c) => c.clone(),
            None => return Ok(CallToolResult::success(vec![Content::text(
                "Device controller not active. Set MQTT_HOST to connect to your MQTT broker.",
            )])),
        };

        // ── Device lookup ─────────────────────────────────────────────────────
        let device = match self.device_registry.get_device(&device_id).await {
            Ok(Some(d)) => d,
            Ok(None) => return Ok(CallToolResult::success(vec![Content::text(format!(
                "Device '{device_id}' not found. Use list_controllable_devices to see available IDs.",
            ))])),
            Err(e) => return Ok(CallToolResult::success(vec![Content::text(format!(
                "Could not look up device: {e}",
            ))])),
        };

        // ── Sensitive device confirmation ─────────────────────────────────────
        if is_sensitive_device(&device) && p.confirmed != Some(true) {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "'{}' is a sensitive device (lock / door / alarm). \
                 Please confirm by resending this request with confirmed=true.",
                device.name
            ))]));
        }

        // ── Actuate ───────────────────────────────────────────────────────────
        let result = if state_key == "power" {
            let on = parse_bool_value(&value);
            ctrl.set_power(&device_id, on).await
        } else {
            let sv = parse_state_value(&value);
            ctrl.set_state(&device_id, &state_key, sv).await
        };

        match result {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Set {state_key}={value} on \"{}\" (id={device_id}).",
                device.name
            ))])),
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Failed to set {state_key} on \"{}\": {e}. \
                 Check the device is online and the MQTT broker is reachable.",
                device.name
            ))])),
        }
    }

    #[tool(description = "Get the current cached state of a controllable device.")]
    async fn get_device_state(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<GetDeviceStateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let device_id = match &params.0.device_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => return Ok(CallToolResult::success(vec![Content::text(
                "Please provide device_id. Use list_controllable_devices to find the correct ID.",
            )])),
        };

        let ctrl = match &self.device_controller {
            Some(c) => c.clone(),
            None => return Ok(CallToolResult::success(vec![Content::text(
                "Device controller not active. Set MQTT_HOST to enable.",
            )])),
        };

        let device_name = self
            .device_registry
            .get_device(&device_id)
            .await
            .ok()
            .flatten()
            .map(|d| d.name)
            .unwrap_or_else(|| device_id.clone());

        match ctrl.query_state(&device_id).await {
            Ok(state) if state.values.is_empty() => Ok(CallToolResult::success(vec![
                Content::text(format!(
                    "No state cached yet for \"{device_name}\". \
                     State is populated once the device publishes a message to the broker.",
                )),
            ])),
            Ok(state) => {
                let mut parts: Vec<String> = state
                    .values
                    .iter()
                    .map(|(k, v)| {
                        let display = match v {
                            StateValue::Bool(b) => if *b { "on" } else { "off" }.to_string(),
                            StateValue::Number(n) => n.to_string(),
                            StateValue::Text(s) => s.clone(),
                        };
                        format!("  {k}: {display}")
                    })
                    .collect();
                parts.sort();
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "State for \"{}\":\n{}",
                    device_name,
                    parts.join("\n")
                ))]))
            }
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Could not query state for \"{device_name}\": {e}",
            ))])),
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn is_sensitive_device(device: &Device) -> bool {
    let haystack = format!("{} {}", device.name, device.device_type).to_lowercase();
    ["lock", "door", "alarm", "gate", "security"]
        .iter()
        .any(|kw| haystack.contains(kw))
}

fn parse_bool_value(v: &str) -> bool {
    matches!(v.to_lowercase().as_str(), "on" | "true" | "1" | "yes")
}

fn parse_state_value(v: &str) -> StateValue {
    if let Ok(n) = v.parse::<f64>() {
        return StateValue::Number(n);
    }
    match v.to_lowercase().as_str() {
        "on" | "true" | "yes" => StateValue::Bool(true),
        "off" | "false" | "no" => StateValue::Bool(false),
        _ => StateValue::Text(v.to_string()),
    }
}

// ── ServerHandler ──────────────────────────────────────────────────────────────

#[tool_handler]
impl ServerHandler for ControlMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-control",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Device Control MCP server — actuation for smart home devices.\n\n\
                 Tools: list_controllable_devices (find device IDs), set_device_state \
                 (power/brightness/mode/etc), get_device_state (read cached state).\n\n\
                 Locks, doors, and alarms require confirmed=true before the command is sent.",
            )
    }
}

// ── Static deps + spawn function ──────────────────────────────────────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct ControlDeps {
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    device_controller: Option<Arc<dyn DeviceController + Send + Sync>>,
}

static CONTROL_DEPS: OnceLock<ControlDeps> = OnceLock::new();

/// Initialize control server dependencies. Call once at startup.
pub fn init_control_deps(
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    device_controller: Option<Arc<dyn DeviceController + Send + Sync>>,
) {
    let _ = CONTROL_DEPS.set(ControlDeps { device_registry, device_controller });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_control_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = CONTROL_DEPS.get().expect("init_control_deps() not called");
    let server = ControlMcpServer::new(
        deps.device_registry.clone(),
        deps.device_controller.clone(),
    );
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-control MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::domain::device::{DeviceCapability, DeviceState, CapabilityKind};
    use pond_core::ports::device_registry::{Device, RegisterDeviceRequest};
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ── Stubs ──────────────────────────────────────────────────────────────────

    fn make_device(id: &str, name: &str, address: Option<&str>, device_type: &str) -> Device {
        Device {
            id: id.to_string(),
            name: name.to_string(),
            device_type: device_type.to_string(),
            hostname: None,
            ip_address: None,
            capabilities: vec![],
            transport: "mqtt".to_string(),
            address: address.map(|a| a.to_string()),
            structured_capabilities: vec![
                DeviceCapability { key: "power".to_string(), kind: CapabilityKind::Toggle },
                DeviceCapability {
                    key: "brightness".to_string(),
                    kind: CapabilityKind::Range { min: 0.0, max: 254.0, step: Some(1.0) },
                },
            ],
            registered_at: "2026-01-01T00:00:00Z".to_string(),
            last_seen: None,
            is_online: true,
        }
    }

    struct StubRegistry {
        devices: Vec<Device>,
    }

    #[async_trait]
    impl DeviceRegistry for StubRegistry {
        async fn register(&self, _: RegisterDeviceRequest) -> anyhow::Result<Device> {
            unimplemented!()
        }
        async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
            Ok(self.devices.clone())
        }
        async fn get_device(&self, id: &str) -> anyhow::Result<Option<Device>> {
            Ok(self.devices.iter().find(|d| d.id == id).cloned())
        }
        async fn unregister(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StubController {
        power_calls: Mutex<Vec<(String, bool)>>,
        state_calls: Mutex<Vec<(String, String, StateValue)>>,
        states: Mutex<HashMap<String, DeviceState>>,
        fail: bool,
    }

    impl StubController {
        fn new() -> Self {
            Self {
                power_calls: Mutex::new(vec![]),
                state_calls: Mutex::new(vec![]),
                states: Mutex::new(HashMap::new()),
                fail: false,
            }
        }
        fn failing() -> Self {
            Self { fail: true, ..Self::new() }
        }
    }

    #[async_trait]
    impl DeviceController for StubController {
        async fn set_power(&self, device_id: &str, on: bool) -> anyhow::Result<()> {
            if self.fail {
                return Err(anyhow::anyhow!("broker unreachable"));
            }
            self.power_calls.lock().unwrap().push((device_id.to_string(), on));
            Ok(())
        }
        async fn set_state(&self, device_id: &str, key: &str, value: StateValue) -> anyhow::Result<()> {
            if self.fail {
                return Err(anyhow::anyhow!("broker unreachable"));
            }
            self.state_calls
                .lock()
                .unwrap()
                .push((device_id.to_string(), key.to_string(), value));
            Ok(())
        }
        async fn query_state(&self, device_id: &str) -> anyhow::Result<DeviceState> {
            Ok(self
                .states
                .lock()
                .unwrap()
                .get(device_id)
                .cloned()
                .unwrap_or_else(|| DeviceState {
                    device_id: device_id.to_string(),
                    values: HashMap::new(),
                }))
        }
        async fn capabilities(&self, _: &str) -> anyhow::Result<Vec<DeviceCapability>> {
            Ok(vec![])
        }
    }

    fn server_with(devices: Vec<Device>, ctrl: Option<Arc<dyn DeviceController + Send + Sync>>) -> ControlMcpServer {
        ControlMcpServer::new(
            Arc::new(StubRegistry { devices }),
            ctrl,
        )
    }

    // ── Tests ──────────────────────────────────────────────────────────────────

    #[test]
    fn server_constructs() {
        let _ = server_with(vec![], None);
    }

    #[test]
    fn is_sensitive_device_detects_keywords() {
        let lock_dev = make_device("1", "front-door-lock", Some("zigbee2mqtt/lock1"), "lock");
        assert!(is_sensitive_device(&lock_dev));

        let alarm = make_device("2", "alarm", Some("zigbee2mqtt/alarm1"), "sensor");
        assert!(is_sensitive_device(&alarm));

        let light = make_device("3", "living-room-light", Some("zigbee2mqtt/lamp1"), "light");
        assert!(!is_sensitive_device(&light));
    }

    #[test]
    fn parse_bool_value_handles_variants() {
        assert!(parse_bool_value("on"));
        assert!(parse_bool_value("ON"));
        assert!(parse_bool_value("true"));
        assert!(parse_bool_value("1"));
        assert!(parse_bool_value("yes"));
        assert!(!parse_bool_value("off"));
        assert!(!parse_bool_value("false"));
        assert!(!parse_bool_value("0"));
    }

    #[test]
    fn parse_state_value_returns_number_for_numeric_string() {
        assert_eq!(parse_state_value("200"), StateValue::Number(200.0));
        assert_eq!(parse_state_value("0.5"), StateValue::Number(0.5));
    }

    #[test]
    fn parse_state_value_returns_bool_for_on_off() {
        assert_eq!(parse_state_value("on"), StateValue::Bool(true));
        assert_eq!(parse_state_value("off"), StateValue::Bool(false));
    }

    #[test]
    fn parse_state_value_returns_text_for_mode_strings() {
        assert_eq!(parse_state_value("heat"), StateValue::Text("heat".to_string()));
        assert_eq!(parse_state_value("cool"), StateValue::Text("cool".to_string()));
    }

    #[tokio::test]
    async fn list_controllable_devices_empty_registry() {
        let srv = server_with(vec![], None);
        let ctx = rmcp::service::RequestContext::new(
            rmcp::model::RequestId::Number(0),
            dummy_peer(),
        );
        let result = srv.list_controllable_devices(ctx).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("No controllable"));
    }

    #[tokio::test]
    async fn list_controllable_devices_excludes_no_address() {
        let d1 = make_device("1", "lamp", Some("zigbee2mqtt/lamp1"), "light");
        let mut d2 = make_device("2", "sensor", None, "sensor");
        d2.address = None;
        let srv = server_with(vec![d1, d2], None);
        let ctx = rmcp::service::RequestContext::new(
            rmcp::model::RequestId::Number(0),
            dummy_peer(),
        );
        let result = srv.list_controllable_devices(ctx).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("lamp"));
        assert!(!text.contains("sensor"));
    }

    #[tokio::test]
    async fn set_device_state_missing_device_id_returns_guidance() {
        let ctrl: Arc<dyn DeviceController + Send + Sync> = Arc::new(StubController::new());
        let srv = server_with(vec![], Some(ctrl));
        let ctx = rmcp::service::RequestContext::new(
            rmcp::model::RequestId::Number(0),
            dummy_peer(),
        );
        let params = rmcp::handler::server::wrapper::Parameters(SetDeviceStateParams {
            device_id: None,
            state_key: Some("power".to_string()),
            value: Some("on".to_string()),
            confirmed: None,
        });
        let result = srv.set_device_state(ctx, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("device_id"));
    }

    #[tokio::test]
    async fn set_device_state_no_controller_returns_guidance() {
        let d = make_device("lamp-1", "lamp", Some("zigbee2mqtt/lamp1"), "light");
        let srv = server_with(vec![d], None);
        let ctx = rmcp::service::RequestContext::new(
            rmcp::model::RequestId::Number(0),
            dummy_peer(),
        );
        let params = rmcp::handler::server::wrapper::Parameters(SetDeviceStateParams {
            device_id: Some("lamp-1".to_string()),
            state_key: Some("power".to_string()),
            value: Some("on".to_string()),
            confirmed: None,
        });
        let result = srv.set_device_state(ctx, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("MQTT_HOST"));
    }

    #[tokio::test]
    async fn set_device_state_sensitive_without_confirmation_blocks() {
        let lock = make_device("lock-1", "front-door-lock", Some("zigbee2mqtt/lock1"), "lock");
        let ctrl: Arc<dyn DeviceController + Send + Sync> = Arc::new(StubController::new());
        let srv = server_with(vec![lock], Some(ctrl));
        let ctx = rmcp::service::RequestContext::new(
            rmcp::model::RequestId::Number(0),
            dummy_peer(),
        );
        let params = rmcp::handler::server::wrapper::Parameters(SetDeviceStateParams {
            device_id: Some("lock-1".to_string()),
            state_key: Some("power".to_string()),
            value: Some("on".to_string()),
            confirmed: None, // not confirmed
        });
        let result = srv.set_device_state(ctx, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("confirmed=true"));
    }

    #[tokio::test]
    async fn set_device_state_sensitive_with_confirmation_actuates() {
        let lock = make_device("lock-1", "front-door-lock", Some("zigbee2mqtt/lock1"), "lock");
        let stub = Arc::new(StubController::new());
        let ctrl: Arc<dyn DeviceController + Send + Sync> = stub.clone();
        let srv = server_with(vec![lock], Some(ctrl));
        let ctx = rmcp::service::RequestContext::new(
            rmcp::model::RequestId::Number(0),
            dummy_peer(),
        );
        let params = rmcp::handler::server::wrapper::Parameters(SetDeviceStateParams {
            device_id: Some("lock-1".to_string()),
            state_key: Some("power".to_string()),
            value: Some("on".to_string()),
            confirmed: Some(true),
        });
        let result = srv.set_device_state(ctx, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("Set power=on"), "got: {text}");
        let calls = stub.power_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], ("lock-1".to_string(), true));
    }

    #[tokio::test]
    async fn set_device_state_power_on_calls_controller() {
        let lamp = make_device("lamp-1", "lamp", Some("zigbee2mqtt/lamp1"), "light");
        let stub = Arc::new(StubController::new());
        let ctrl: Arc<dyn DeviceController + Send + Sync> = stub.clone();
        let srv = server_with(vec![lamp], Some(ctrl));
        let ctx = rmcp::service::RequestContext::new(
            rmcp::model::RequestId::Number(0),
            dummy_peer(),
        );
        let params = rmcp::handler::server::wrapper::Parameters(SetDeviceStateParams {
            device_id: Some("lamp-1".to_string()),
            state_key: Some("power".to_string()),
            value: Some("on".to_string()),
            confirmed: None,
        });
        let _ = srv.set_device_state(ctx, params).await.unwrap();
        let calls = stub.power_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], ("lamp-1".to_string(), true));
    }

    #[tokio::test]
    async fn set_device_state_brightness_encodes_number() {
        let lamp = make_device("lamp-1", "lamp", Some("zigbee2mqtt/lamp1"), "light");
        let stub = Arc::new(StubController::new());
        let ctrl: Arc<dyn DeviceController + Send + Sync> = stub.clone();
        let srv = server_with(vec![lamp], Some(ctrl));
        let ctx = rmcp::service::RequestContext::new(
            rmcp::model::RequestId::Number(0),
            dummy_peer(),
        );
        let params = rmcp::handler::server::wrapper::Parameters(SetDeviceStateParams {
            device_id: Some("lamp-1".to_string()),
            state_key: Some("brightness".to_string()),
            value: Some("200".to_string()),
            confirmed: None,
        });
        let _ = srv.set_device_state(ctx, params).await.unwrap();
        let calls = stub.state_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, "brightness");
        assert_eq!(calls[0].2, StateValue::Number(200.0));
    }

    #[tokio::test]
    async fn set_device_state_controller_failure_returns_guidance() {
        let lamp = make_device("lamp-1", "lamp", Some("zigbee2mqtt/lamp1"), "light");
        let ctrl: Arc<dyn DeviceController + Send + Sync> = Arc::new(StubController::failing());
        let srv = server_with(vec![lamp], Some(ctrl));
        let ctx = rmcp::service::RequestContext::new(
            rmcp::model::RequestId::Number(0),
            dummy_peer(),
        );
        let params = rmcp::handler::server::wrapper::Parameters(SetDeviceStateParams {
            device_id: Some("lamp-1".to_string()),
            state_key: Some("power".to_string()),
            value: Some("off".to_string()),
            confirmed: None,
        });
        let result = srv.set_device_state(ctx, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("Failed") || text.contains("broker"), "got: {text}");
    }

    #[tokio::test]
    async fn get_device_state_empty_cache_returns_guidance() {
        let lamp = make_device("lamp-1", "lamp", Some("zigbee2mqtt/lamp1"), "light");
        let ctrl: Arc<dyn DeviceController + Send + Sync> = Arc::new(StubController::new());
        let srv = server_with(vec![lamp], Some(ctrl));
        let ctx = rmcp::service::RequestContext::new(
            rmcp::model::RequestId::Number(0),
            dummy_peer(),
        );
        let params = rmcp::handler::server::wrapper::Parameters(GetDeviceStateParams {
            device_id: Some("lamp-1".to_string()),
        });
        let result = srv.get_device_state(ctx, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("No state cached") || text.contains("lamp"), "got: {text}");
    }

    // ── Test helpers ───────────────────────────────────────────────────────────

    fn extract_text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| {
                if let rmcp::model::RawContent::Text(t) = &c.raw {
                    Some(t.text.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("")
    }

    fn dummy_peer() -> rmcp::service::Peer<rmcp::RoleServer> {
        use rmcp::ServiceExt;
        let (_client, server) = tokio::io::duplex(64);
        rmcp::service::serve_directly(crate::system::SystemMcpServer::new(), server, None).peer().clone()
    }
}
