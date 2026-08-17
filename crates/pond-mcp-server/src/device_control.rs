//! Device Control MCP Server — actuation.
//!
//! Provides one tool: `set_device_state`, which actuates a device through the
//! [`DeviceControlPort`] (power / brightness / target temperature / lock). The
//! agent and the desktop Hub (via `POST /api/v1/tools/invoke`) both reach
//! devices through this single tool surface. Backends are pluggable behind the
//! port (logging stub today; MQTT/HTTP/IR or a Home-Assistant MCP-client later).

use pond_core::user_data::ports::device_control::{
    DeviceControlPort, DeviceDescription, ValueSpec,
};
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
pub struct DescribeDeviceParams {
    /// Device id, name, or a natural reference like "the fan".
    pub device_id: String,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

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
    /// A named setting on an appliance — wash cycle, spin speed, temperature
    /// level. Both the name and the value come from describe_device; they are
    /// the device's own words, so do not translate or abbreviate them.
    #[serde(default)]
    pub setting: Option<String>,
    /// The value for `setting`. Ignored unless `setting` is given.
    #[serde(default)]
    pub setting_value: Option<String>,
    /// start, stop, pause or resume, for a device that runs cycles.
    #[serde(default)]
    pub operation: Option<String>,
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

/// The description as a sentence a model can act on. JSON would be smaller and
/// worse: the point is that the next tool call is obvious from reading it.
fn render_description(d: &DeviceDescription) -> String {
    let mut out = format!("{} ({})", d.device_id, d.device_type);

    if d.capabilities.is_empty() {
        out.push_str("\n  Cannot be controlled — nothing to set on this device.");
    } else {
        out.push_str("\n  Accepts:");
        for capability in &d.capabilities {
            out.push_str(&format!(
                "\n    {} — {}",
                capability.verb,
                render_value(&capability.value)
            ));
        }
    }

    if d.sensors.is_empty() {
        out.push_str("\n  Measures: nothing.");
    } else {
        out.push_str("\n  Measures:");
        for sensor in &d.sensors {
            out.push_str(&format!("\n    {} ({})", sensor.sensor_type, sensor.unit));
        }
    }
    out
}

fn render_value(value: &ValueSpec) -> String {
    match value {
        ValueSpec::Boolean => "true or false".to_string(),
        ValueSpec::Percent => "0-100 percent".to_string(),
        ValueSpec::Color => "hue 0-360 with saturation 0-100".to_string(),
        ValueSpec::Enum { values } => format!("one of: {}", values.join(", ")),
        ValueSpec::Number { min, max, unit } => {
            let unit = unit.as_deref().unwrap_or("");
            match (min, max) {
                (Some(lo), Some(hi)) => format!("a number from {lo} to {hi} {unit}")
                    .trim_end()
                    .to_string(),
                (Some(lo), None) => format!("a number from {lo} {unit}").trim_end().to_string(),
                (None, Some(hi)) => format!("a number up to {hi} {unit}").trim_end().to_string(),
                // No stated limits: say so rather than implying a range.
                (None, None) => format!("a number in {unit}").trim_end().to_string(),
            }
        }
    }
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
        description = "What a device can be told to do and what it measures: the verbs it \
                       accepts, the values each takes (fan modes, temperature limits), and \
                       its sensors. Consult this before driving a device you have not driven \
                       before, rather than attempting a verb to find out whether it works. \
                       device_id: id, name, or natural ref like \"the fan\"."
    )]
    async fn describe_device(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<DescribeDeviceParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("describe_device");
        let Parameters(p) = params;
        let device_id = p.device_id.trim();

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
                tracing::warn!(error = %e, "describe_device: device list unavailable");
                device_id.to_string()
            }
        };

        match self.control.describe(&device_id).await {
            Ok(description) => Ok(CallToolResult::success(vec![Content::text(
                render_description(&description),
            )])),
            Err(e) => Ok(guidance(format!("Couldn't describe '{device_id}': {e}"))),
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
            && p.setting.is_none()
            && p.operation.is_none()
            && p.position.is_none()
        {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No change requested for '{device_id}'. Specify one of: power (on/off), \
                 brightness (0-100), target_temp (°C), locked (true/false), hue (0-360) + \
                 saturation (0-100), fan_speed (0-100), fan_mode (off/low/medium/high/on/auto/\
                 smart), setting + setting_value (appliance settings such as a wash \
                 cycle or spin speed — see describe_device), operation (start/stop/\
                 pause/resume), or position (0-100 percent open)."
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
        if let Some(setting) = p.setting.as_deref() {
            // Both halves or neither: a setting with no value is a question, not
            // an instruction, and guessing which value was meant is how a wash
            // ends up on the wrong cycle.
            let Some(value) = p.setting_value.as_deref() else {
                return Ok(guidance(format!(
                    "Which value for '{setting}' on '{device_id}'? Pass setting_value. \
                     describe_device lists what it accepts."
                )));
            };
            match self.control.set_mode(device_id, setting, value).await {
                Ok(_) => applied.push(format!("{setting}={value}")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set {setting} on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(operation) = p.operation.as_deref() {
            match self.control.set_operation(device_id, operation).await {
                Ok(_) => applied.push(format!("operation={operation}")),
                Err(e) => return Ok(guidance(format!("Couldn't {operation} '{device_id}': {e}"))),
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
    crate::serve_builtin("giap-device-control", server, reader, writer);
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::user_data::ports::device_control::{DeviceControlOutcome, DeviceStatePatch};

    use pond_core::user_data::ports::device_control::{Capability, SensorSpec};

    fn spec(verb: &str, value: ValueSpec) -> Capability {
        Capability {
            verb: verb.to_string(),
            value,
        }
    }

    /// What the model reads. The whole point of the tool is that the next call is
    /// obvious from the text, so the text is what gets asserted.
    #[test]
    fn a_description_names_the_values_a_device_accepts() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-18".into(),
            device_type: "fan".into(),
            capabilities: vec![
                spec("power", ValueSpec::Boolean),
                spec("fan_speed", ValueSpec::Percent),
                spec(
                    "fan_mode",
                    ValueSpec::Enum {
                        values: vec!["off".into(), "low".into(), "high".into()],
                    },
                ),
            ],
            sensors: vec![],
        });

        assert!(rendered.contains("matter-18 (fan)"), "{rendered}");
        // The modes are the reason this tool exists: "fan_mode" alone sends the
        // model back to guessing which words are accepted.
        assert!(rendered.contains("one of: off, low, high"), "{rendered}");
        assert!(rendered.contains("0-100 percent"), "{rendered}");
        assert!(rendered.contains("Measures: nothing."), "{rendered}");
    }

    #[test]
    fn a_stated_range_is_shown_and_an_unstated_one_is_not_invented() {
        let stated = render_description(&DeviceDescription {
            device_id: "matter-30".into(),
            device_type: "thermostat".into(),
            capabilities: vec![spec(
                "target_temp",
                ValueSpec::Number {
                    min: Some(7.0),
                    max: Some(30.0),
                    unit: Some("C".into()),
                },
            )],
            sensors: vec![],
        });
        assert!(stated.contains("from 7 to 30 C"), "{stated}");

        let silent = render_description(&DeviceDescription {
            device_id: "matter-31".into(),
            device_type: "thermostat".into(),
            capabilities: vec![spec(
                "target_temp",
                ValueSpec::Number {
                    min: None,
                    max: None,
                    unit: Some("C".into()),
                },
            )],
            sensors: vec![],
        });
        assert!(silent.contains("a number in C"), "{silent}");
        assert!(!silent.contains("from"), "no range is implied: {silent}");
    }

    /// A sensor is describable before it has ever reported, which is the question
    /// "what does this measure?" that readings alone could not answer.
    #[test]
    fn a_sensor_lists_what_it_measures_with_units() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-40".into(),
            device_type: "sensor".into(),
            capabilities: vec![],
            sensors: vec![
                SensorSpec {
                    sensor_type: "carbon_dioxide".into(),
                    unit: "ppm".into(),
                },
                SensorSpec {
                    sensor_type: "pm2_5".into(),
                    unit: "ug/m3".into(),
                },
            ],
        });

        assert!(rendered.contains("carbon_dioxide (ppm)"), "{rendered}");
        assert!(rendered.contains("pm2_5 (ug/m3)"), "{rendered}");
        // And it says plainly that there is nothing to drive, rather than leaving
        // the model to infer it from an empty list.
        assert!(rendered.contains("Cannot be controlled"), "{rendered}");
    }

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
