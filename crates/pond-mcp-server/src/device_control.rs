//! Device Control MCP Server — actuation.
//!
//! Provides one tool: `set_device_state`, which actuates a device through the
//! [`DeviceControlPort`] (power / brightness / target temperature / lock). The
//! agent and the desktop Hub (via `POST /api/v1/tools/invoke`) both reach
//! devices through this single tool surface. Backends are pluggable behind the
//! port (logging stub today; MQTT/HTTP/IR or a Home-Assistant MCP-client later).

use pond_core::user_data::ports::device_control::DeviceControlPort;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry};
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
use std::collections::HashMap;
use std::sync::Arc;

// ── Parameter struct ─────────────────────────────────────────────────────────
// All params optional with serde(default) + a flatten extra absorber so a small
// model sending `{}` (or unexpected fields) never breaks deserialization.

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SetDeviceStateParams {
    /// Device id or name.
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub power: Option<bool>,
    /// 0-100 percent.
    #[serde(default)]
    pub brightness: Option<u8>,
    /// Degrees Celsius.
    #[serde(default)]
    pub target_temp: Option<f32>,
    #[serde(default)]
    pub locked: Option<bool>,
    /// Hue degrees 0-360.
    #[serde(default)]
    pub hue: Option<u16>,
    /// 0-100 percent.
    #[serde(default)]
    pub saturation: Option<u8>,
    /// 0-100 percent.
    #[serde(default)]
    pub fan_speed: Option<u8>,
    /// Fan mode: off, low, medium, high, on, auto, smart. Use this rather than
    /// fan_speed when the user names a mode — auto and smart have no percentage.
    #[serde(default)]
    pub fan_mode: Option<String>,
    /// 0-100 percent open (100=fully open).
    #[serde(default)]
    pub position: Option<u8>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DeviceControlMcpServer {
    control: Arc<dyn DeviceControlPort + Send + Sync>,
    /// Registry used to resolve natural references ("the light", a friendly
    /// name) to the device id backends expect — what lets the user say
    /// "turn off the light" instead of quoting an internal id.
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl DeviceControlMcpServer {
    pub fn new(
        control: Arc<dyn DeviceControlPort + Send + Sync>,
        registry: Arc<dyn DeviceRegistry + Send + Sync>,
    ) -> Self {
        Self {
            control,
            registry,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Set smart-device state: power, brightness, target_temp, lock, colour, fan, position. device_id: id, name, or natural ref like \"the light\"."
    )]
    async fn set_device_state(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SetDeviceStateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let device_id = p.device_id.trim();

        if device_id.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "Which device? Provide `device_id` (the device name or id) plus what to change: \
                 power (on/off), brightness (0-100), target_temp (°C), or locked (true/false).",
            )]));
        }
        if p.power.is_none()
            && p.brightness.is_none()
            && p.target_temp.is_none()
            && p.locked.is_none()
            && p.hue.is_none()
            && p.saturation.is_none()
            && p.fan_speed.is_none()
            && p.fan_mode.is_none()
            && p.position.is_none()
        {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No change requested for '{device_id}'. Specify one of: power (on/off), \
                 brightness (0-100), target_temp (°C), locked (true/false), hue (0-360) + \
                 saturation (0-100), fan_speed (0-100), fan_mode (off/low/medium/high/on/auto/\
                 smart), or position (0-100 percent open)."
            ))]));
        }

        // Resolve natural references ("the light", a friendly name) to the
        // registered device id — backends receive ids, users speak names.
        let device_id = match self.registry.list_devices().await {
            Ok(devices) => match resolve_device(device_id, &devices) {
                DeviceResolution::Resolved(id) => id,
                DeviceResolution::Ambiguous(names) => {
                    return Ok(guidance(format!(
                        "'{device_id}' matches several devices: {}. Which one?",
                        names.join(", ")
                    )));
                }
                DeviceResolution::NotFound => {
                    let known: Vec<String> = devices.iter().map(|d| d.name.clone()).collect();
                    return Ok(guidance(if known.is_empty() {
                        "No devices are registered yet.".to_string()
                    } else {
                        format!(
                            "No device matches '{device_id}'. Registered devices: {}.",
                            known.join(", ")
                        )
                    }));
                }
            },
            Err(e) => {
                // Registry unavailable: pass the raw reference through so a
                // backend that understands it can still act.
                tracing::warn!(error = %e, "device resolution unavailable; using raw id");
                device_id.to_string()
            }
        };
        let device_id = device_id.as_str();

        let mut applied: Vec<String> = Vec::new();

        if let Some(on) = p.power {
            match self.control.set_power(device_id, on).await {
                Ok(_) => applied.push(format!("power={}", if on { "on" } else { "off" })),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set power on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(b) = p.brightness {
            let pct = b.min(100);
            match self.control.set_brightness(device_id, pct).await {
                Ok(_) => applied.push(format!("brightness={pct}%")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set brightness on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(t) = p.target_temp {
            match self.control.set_target_temp(device_id, t).await {
                Ok(_) => applied.push(format!("target_temp={t}°C")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set temperature on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(locked) = p.locked {
            match self.control.set_locked(device_id, locked).await {
                Ok(_) => applied.push(format!("locked={locked}")),
                Err(e) => return Ok(guidance(format!("Couldn't (un)lock '{device_id}': {e}"))),
            }
        }
        // Colour: hue and saturation are one action. If only one is given, keep
        // the other at a sensible default (full saturation for a bare hue).
        if p.hue.is_some() || p.saturation.is_some() {
            let hue = p.hue.unwrap_or(0) % 360;
            let sat = p.saturation.unwrap_or(100).min(100);
            match self.control.set_color(device_id, hue, sat).await {
                Ok(_) => applied.push(format!("colour=hue {hue}°/sat {sat}%")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set colour on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(speed) = p.fan_speed {
            let pct = speed.min(100);
            match self.control.set_fan_speed(device_id, pct).await {
                Ok(_) => applied.push(format!("fan_speed={pct}%")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set fan speed on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(mode) = p.fan_mode.as_deref() {
            match self.control.set_fan_mode(device_id, mode).await {
                Ok(_) => applied.push(format!("fan_mode={mode}")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set fan mode on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(open) = p.position {
            let pct = open.min(100);
            match self.control.set_position(device_id, pct).await {
                Ok(_) => applied.push(format!("position={pct}% open")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set position on '{device_id}': {e}"
                    )))
                }
            }
        }

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Set {device_id}: {}.",
            applied.join(", ")
        ))]))
    }
}

/// Error-as-guidance: the LLM reads this and adapts (per the MCP server standard).
fn guidance(msg: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(msg)])
}

// ── Natural device resolution (pure, unit-tested) ────────────────────────────

/// Outcome of resolving a user/LLM device reference against the registry.
#[derive(Debug, PartialEq)]
enum DeviceResolution {
    /// The registered id to hand to the control backend.
    Resolved(String),
    /// Multiple devices plausibly match — names for a disambiguation prompt.
    Ambiguous(Vec<String>),
    NotFound,
}

/// Normalise a spoken reference: lowercase, strip a leading article and a
/// trailing plural 's' ("the lights" → "light").
fn normalize_reference(input: &str) -> String {
    let lower = input.trim().to_lowercase();
    let stripped = lower
        .strip_prefix("the ")
        .or_else(|| lower.strip_prefix("my "))
        .unwrap_or(&lower);
    stripped.strip_suffix('s').unwrap_or(stripped).to_string()
}

/// Resolve `input` to a registered device id. Matching tiers, strictest
/// first — the first tier with hits decides:
/// 1. exact id (backends' canonical form, e.g. `"matter-2"`)
/// 2. exact name, case-insensitive ("Living Room Light")
/// 3. name containing the reference ("living room" → Living Room Light)
/// 4. device type ("the light" → the only device_type == "light")
///
/// A unique hit resolves; several hits in the same tier are ambiguous.
fn resolve_device(input: &str, devices: &[Device]) -> DeviceResolution {
    let raw = input.trim();
    if let Some(device) = devices.iter().find(|d| d.id == raw) {
        return DeviceResolution::Resolved(device.id.clone());
    }

    let lower = raw.to_lowercase();
    let tiers: [Vec<&Device>; 3] = [
        devices
            .iter()
            .filter(|d| d.name.to_lowercase() == lower)
            .collect(),
        devices
            .iter()
            .filter(|d| d.name.to_lowercase().contains(&lower))
            .collect(),
        devices
            .iter()
            .filter(|d| d.device_type.to_lowercase() == normalize_reference(raw))
            .collect(),
    ];
    for tier in tiers {
        match tier.as_slice() {
            [] => continue,
            [device] => return DeviceResolution::Resolved(device.id.clone()),
            many => {
                return DeviceResolution::Ambiguous(many.iter().map(|d| d.name.clone()).collect())
            }
        }
    }
    DeviceResolution::NotFound
}

#[tool_handler]
impl ServerHandler for DeviceControlMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-device-control",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Device Control MCP server — actuate smart devices.\n\n\
                 Tool: set_device_state (power on/off, brightness 0-100, target_temp °C, \
                 lock/unlock). device_id accepts the registered id, the device name, or a \
                 natural reference like \"the light\"; plus the field(s) to change.",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct DeviceControlDeps {
    control: Arc<dyn DeviceControlPort + Send + Sync>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
}

static DEVICE_CONTROL_DEPS: OnceLock<DeviceControlDeps> = OnceLock::new();

/// Initialize device-control server dependencies. Call once at startup.
pub fn init_device_control_deps(
    control: Arc<dyn DeviceControlPort + Send + Sync>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
) {
    let _ = DEVICE_CONTROL_DEPS.set(DeviceControlDeps { control, registry });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_device_control_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = DEVICE_CONTROL_DEPS
        .get()
        .expect("init_device_control_deps() not called");
    let server = DeviceControlMcpServer::new(deps.control.clone(), deps.registry.clone());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-device-control MCP server failed: {e}"),
        }
    });
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::user_data::ports::device_control::{DeviceControlOutcome, DeviceStatePatch};

    struct StubControl;
    #[async_trait]
    impl DeviceControlPort for StubControl {
        async fn set_power(&self, id: &str, on: bool) -> anyhow::Result<DeviceControlOutcome> {
            Ok(DeviceControlOutcome::new(
                id,
                DeviceStatePatch {
                    on: Some(on),
                    ..Default::default()
                },
            ))
        }
        async fn set_brightness(
            &self,
            id: &str,
            percent: u8,
        ) -> anyhow::Result<DeviceControlOutcome> {
            Ok(DeviceControlOutcome::new(
                id,
                DeviceStatePatch {
                    brightness: Some(percent),
                    ..Default::default()
                },
            ))
        }
        async fn set_target_temp(
            &self,
            id: &str,
            celsius: f32,
        ) -> anyhow::Result<DeviceControlOutcome> {
            Ok(DeviceControlOutcome::new(
                id,
                DeviceStatePatch {
                    target_temp: Some(celsius),
                    ..Default::default()
                },
            ))
        }
        async fn set_locked(&self, id: &str, locked: bool) -> anyhow::Result<DeviceControlOutcome> {
            Ok(DeviceControlOutcome::new(
                id,
                DeviceStatePatch {
                    locked: Some(locked),
                    ..Default::default()
                },
            ))
        }
    }

    /// Inline empty registry (this crate's tests use local stubs, not
    /// pond-core's feature-gated mocks).
    struct EmptyRegistry;
    #[async_trait]
    impl DeviceRegistry for EmptyRegistry {
        async fn register(
            &self,
            _: pond_core::user_data::ports::device_registry::RegisterDeviceRequest,
        ) -> anyhow::Result<Device> {
            anyhow::bail!("not used")
        }
        async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
            Ok(vec![])
        }
        async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
            Ok(None)
        }
        async fn unregister(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_server() -> DeviceControlMcpServer {
        DeviceControlMcpServer::new(Arc::new(StubControl), Arc::new(EmptyRegistry))
    }

    #[test]
    fn server_constructs() {
        let _server = test_server();
    }

    // ── resolve_device: natural references → registered ids ─────────────────

    fn device(id: &str, name: &str, device_type: &str) -> Device {
        Device {
            id: id.to_string(),
            name: name.to_string(),
            device_type: device_type.to_string(),
            hostname: None,
            ip_address: None,
            capabilities: vec![],
            registered_at: "2026-01-01T00:00:00Z".to_string(),
            last_seen: None,
            is_online: true,
            room: None,
        }
    }

    /// The production ask: "turn off the light" resolves to the one light,
    /// whatever it is called and whichever backend registered it.
    #[test]
    fn the_light_resolves_by_type_when_unique() {
        let devices = [
            device("matter-2", "Living Room Light", "light"),
            device("matter-9", "Front Door", "lock"),
        ];
        for spoken in ["the light", "light", "The Lights", "my light"] {
            assert_eq!(
                resolve_device(spoken, &devices),
                DeviceResolution::Resolved("matter-2".into()),
                "failed for {spoken:?}"
            );
        }
    }

    #[test]
    fn ids_names_and_partial_names_resolve_in_tier_order() {
        let devices = [
            device("matter-2", "Living Room Light", "light"),
            device("matter-3", "Bedroom Light", "light"),
        ];
        // Exact id always wins.
        assert_eq!(
            resolve_device("matter-3", &devices),
            DeviceResolution::Resolved("matter-3".into())
        );
        // Case-insensitive exact name.
        assert_eq!(
            resolve_device("living room light", &devices),
            DeviceResolution::Resolved("matter-2".into())
        );
        // Unique partial name.
        assert_eq!(
            resolve_device("bedroom", &devices),
            DeviceResolution::Resolved("matter-3".into())
        );
    }

    #[test]
    fn ambiguous_and_unknown_references_ask_instead_of_guessing() {
        let devices = [
            device("matter-2", "Living Room Light", "light"),
            device("matter-3", "Bedroom Light", "light"),
        ];
        // Two lights: "the light" must NOT pick one silently.
        match resolve_device("the light", &devices) {
            DeviceResolution::Ambiguous(names) => assert_eq!(names.len(), 2),
            other => panic!("expected ambiguity, got {other:?}"),
        }
        assert_eq!(
            resolve_device("the thermostat", &devices),
            DeviceResolution::NotFound
        );
    }

    #[test]
    fn params_default_from_empty_object() {
        let p: SetDeviceStateParams = serde_json::from_str("{}").unwrap();
        assert!(p.device_id.is_empty());
        assert!(p.power.is_none());
    }

    #[test]
    fn params_absorb_unexpected_fields() {
        let p: SetDeviceStateParams =
            serde_json::from_str(r#"{"device_id":"lamp","power":true,"surprise":1}"#).unwrap();
        assert_eq!(p.device_id, "lamp");
        assert_eq!(p.power, Some(true));
        assert!(p.extra.contains_key("surprise"));
    }
}
