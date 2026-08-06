//! python-matter-server wire protocol (schema 11) — pure functions and types,
//! so every mapping is unit-testable without a WebSocket.
//!
//! The server speaks JSON over a WebSocket:
//! - requests:  `{"message_id": "…", "command": "…", "args": {…}}`
//! - responses: `{"message_id": "…", "result": …}` or
//!   `{"message_id": "…", "error_code": N, "details": "…"}`
//! - events:    `{"event": "…", "data": …}` (e.g. `attribute_updated` with
//!   `[node_id, "endpoint/cluster/attribute", value]`)
//!
//! Node attributes arrive as a flat map keyed `"endpoint/cluster/attribute"`
//! (decimal), which is what all the cluster lookups below parse.

use std::collections::BTreeMap;

use chrono::Utc;
use pond_core::user_data::domain::sensor::SensorReading;
use pond_core::user_data::ports::device_registry::Device;
use serde::Deserialize;
use serde_json::{json, Value};

// ── Matter cluster ids (decimal, per the Matter spec) ────────────────────────
pub const CLUSTER_ON_OFF: u32 = 6;
pub const CLUSTER_LEVEL_CONTROL: u32 = 8;
pub const CLUSTER_BASIC_INFORMATION: u32 = 40;
/// NodeLabel — the writable, user-assigned name on Basic Information. Preferred
/// by [`node_to_device`] over the vendor ProductName.
pub const ATTR_NODE_LABEL: u32 = 5;
/// Descriptor — every endpoint has one, and its DeviceTypeList states what the
/// endpoint *is*. The authority on device type: clusters describe what can be
/// driven, which is a different question (an On/Off plug and an On/Off bulb are
/// the same cluster).
pub const CLUSTER_DESCRIPTOR: u32 = 29;
pub const CLUSTER_AIR_QUALITY: u32 = 91;
pub const CLUSTER_SMOKE_CO_ALARM: u32 = 92;
pub const CLUSTER_BOOLEAN_STATE: u32 = 69;
pub const CLUSTER_DOOR_LOCK: u32 = 257;
pub const CLUSTER_WINDOW_COVERING: u32 = 258;
pub const CLUSTER_FAN_CONTROL: u32 = 514;
pub const CLUSTER_THERMOSTAT: u32 = 513;
pub const CLUSTER_COLOR_CONTROL: u32 = 768;
pub const CLUSTER_ILLUMINANCE: u32 = 1024;
pub const CLUSTER_PRESSURE: u32 = 1027;
pub const CLUSTER_FLOW: u32 = 1028;
pub const CLUSTER_TEMPERATURE: u32 = 1026;
pub const CLUSTER_HUMIDITY: u32 = 1029;
pub const CLUSTER_OCCUPANCY: u32 = 1030;

// Resource monitoring — the air purifier's two filters. Same cluster shape,
// one instance per filter.
pub const CLUSTER_HEPA_FILTER: u32 = 113;
pub const CLUSTER_ACTIVATED_CARBON_FILTER: u32 = 114;
/// Remaining life as a percentage.
pub const ATTR_FILTER_CONDITION: u32 = 0;
/// 0 = OK, 1 = Warning, 2 = Critical.
pub const ATTR_FILTER_CHANGE_INDICATION: u32 = 2;

// Concentration measurement — one cluster per substance, all reporting
// `MeasuredValue` on attribute 0.
//
// Units are the defaults for each substance. The device also publishes a
// `MeasurementUnit` attribute (8) which is authoritative and which GIAP does
// not read yet: a device reporting CO2 in ppb rather than ppm would be
// labelled wrongly. Worth reading before this is trusted for anything but
// display.
pub const CLUSTER_CO: u32 = 1036;
pub const CLUSTER_CO2: u32 = 1037;
pub const CLUSTER_NO2: u32 = 1043;
pub const CLUSTER_OZONE: u32 = 1045;
pub const CLUSTER_PM25: u32 = 1066;
pub const CLUSTER_FORMALDEHYDE: u32 = 1067;
pub const CLUSTER_PM1: u32 = 1068;
pub const CLUSTER_PM10: u32 = 1069;
pub const CLUSTER_TVOC: u32 = 1070;
pub const CLUSTER_RADON: u32 = 1071;
/// `MeasurementUnit` — what the device says its concentrations are in. The
/// substance defaults below are only a guess until this is read.
pub const ATTR_MEASUREMENT_UNIT: u32 = 8;

/// Is this one of the concentration clusters, which publish their own unit?
fn is_concentration_cluster(cluster: u32) -> bool {
    matches!(
        cluster,
        CLUSTER_CO
            | CLUSTER_CO2
            | CLUSTER_NO2
            | CLUSTER_OZONE
            | CLUSTER_PM25
            | CLUSTER_FORMALDEHYDE
            | CLUSTER_PM1
            | CLUSTER_PM10
            | CLUSTER_TVOC
            | CLUSTER_RADON
    )
}

/// Matter's `MeasurementUnitEnum`, as a unit string.
fn measurement_unit_name(code: u64) -> Option<&'static str> {
    Some(match code {
        0 => "ppm",
        1 => "ppb",
        2 => "ppt",
        3 => "mg/m3",
        4 => "ug/m3",
        5 => "ng/m3",
        6 => "/m3",
        7 => "Bq/m3",
        _ => return None,
    })
}

/// The unit the device itself declares for the concentration at `path`, if it
/// declares one.
///
/// [`sensor_reading_from_update`] is pure over a single attribute and cannot
/// see this — the unit lives on a sibling attribute of the same cluster. So the
/// default it applies is the substance's conventional one, and this corrects it
/// wherever a node is in hand. A device reporting CO2 in ppb was otherwise
/// labelled ppm: harmless on screen, wrong the moment a rule compares it.
pub fn declared_unit_for(node: &MatterNode, path: &str) -> Option<&'static str> {
    let mut parts = path.split('/');
    let endpoint: u16 = parts.next()?.parse().ok()?;
    let cluster: u32 = parts.next()?.parse().ok()?;
    if !is_concentration_cluster(cluster) {
        return None;
    }
    let code = node
        .attributes
        .get(&format!("{endpoint}/{cluster}/{ATTR_MEASUREMENT_UNIT}"))?
        .as_u64()?;
    measurement_unit_name(code)
}

/// Thermostat `OccupiedHeatingSetpoint` attribute id.
pub const ATTR_OCCUPIED_HEATING_SETPOINT: u32 = 18;
/// FanControl `PercentSetting` attribute id — a 0–100 write, no command.
pub const ATTR_FAN_PERCENT_SETTING: u32 = 2;
/// FanControl `FanMode` attribute id. A fan has no On/Off cluster to switch, so
/// this is where its power lives.
pub const ATTR_FAN_MODE: u32 = 0;
/// `FanMode` values GIAP writes. The enum also carries Low/Medium/High (1–3),
/// which speed changes go through `PercentSetting` for instead — the server
/// keeps the two in step, so there is no need to pick a discrete step here.
pub const FAN_MODE_OFF: u8 = 0;
pub const FAN_MODE_ON: u8 = 4;

/// `FanMode` by the name a user says it. Auto and Smart are not points on the
/// percentage scale — they hand the choice back to the device — which is why a
/// fan needs modes as well as a speed.
pub fn fan_mode_from_name(name: &str) -> Option<u8> {
    match name.trim().to_lowercase().as_str() {
        "off" => Some(FAN_MODE_OFF),
        "low" => Some(1),
        "medium" | "med" => Some(2),
        "high" => Some(3),
        "on" => Some(FAN_MODE_ON),
        "auto" => Some(5),
        "smart" => Some(6),
        _ => None,
    }
}

/// Matter device type ids (Descriptor DeviceTypeList), grouped onto the GIAP
/// types the UI has icons for. Ids are from the Matter Device Library; the
/// grouping is ours — a dishwasher and a washing machine are both "appliance"
/// as far as anything GIAP shows or says is concerned.
const DEVICE_TYPES: &[(u32, &str)] = &[
    // Lighting
    (0x0100, "light"), // On/Off Light
    (0x0101, "light"), // Dimmable Light
    (0x010C, "light"), // Colour Temperature Light
    (0x010D, "light"), // Extended Colour Light
    // Plugs — the pair of clusters alone cannot tell these from a bulb, which
    // is why a plug used to arrive wearing a lightbulb.
    (0x010A, "plug"), // On/Off Plug-in Unit
    (0x010B, "plug"), // Dimmable Plug-in Unit
    // Closures
    (0x000A, "lock"),     // Door Lock
    (0x0202, "covering"), // Window Covering
    // Climate and air
    (0x0301, "thermostat"), // Thermostat
    (0x0072, "thermostat"), // Room Air Conditioner
    (0x002B, "fan"),        // Fan
    (0x002C, "air"),        // Air Purifier
    // Sensors
    (0x0015, "sensor"), // Contact Sensor
    (0x002D, "sensor"), // Air Quality Sensor
    (0x0106, "sensor"), // Light Sensor
    (0x0107, "sensor"), // Occupancy Sensor
    (0x0302, "sensor"), // Temperature Sensor
    (0x0305, "sensor"), // Pressure Sensor
    (0x0306, "sensor"), // Flow Sensor
    (0x0307, "sensor"), // Humidity Sensor
    // An alarm is not a sensor to a user: it is the thing that wakes them.
    (0x0076, "alarm"), // Smoke/CO Alarm
    // Appliances
    (0x0073, "appliance"), // Laundry Washer
    (0x0075, "appliance"), // Dishwasher
    (0x0074, "vacuum"),    // Robotic Vacuum Cleaner
    (0x0303, "pump"),      // Pump
    // Media
    (0x0023, "media"), // Casting Video Player
    (0x0028, "media"), // Basic Video Player
];

/// The GIAP device type stated by the node itself, if it says.
///
/// Endpoint 0 is the Root Node (0x0016) on every device and never describes the
/// application, so it is skipped. The first application endpoint that names a
/// type GIAP knows wins; a composed device (a fan inside an air purifier) is
/// reported as whatever its first endpoint claims, which is what its own UI
/// calls it.
pub fn device_type_from_descriptor(node: &MatterNode) -> Option<&'static str> {
    let mut endpoints: Vec<(u16, &Value)> = node
        .attributes
        .iter()
        .filter_map(|(key, value)| {
            let mut parts = key.split('/');
            let endpoint: u16 = parts.next()?.parse().ok()?;
            let cluster: u32 = parts.next()?.parse().ok()?;
            let attribute: u32 = parts.next()?.parse().ok()?;
            (cluster == CLUSTER_DESCRIPTOR && attribute == 0 && endpoint != 0)
                .then_some((endpoint, value))
        })
        .collect();
    endpoints.sort_by_key(|(endpoint, _)| *endpoint);

    endpoints.into_iter().find_map(|(_, value)| {
        // DeviceTypeList entries are structs keyed by field number; "0" is the
        // device type id, "1" its revision.
        value.as_array()?.iter().find_map(|entry| {
            let id = entry.get("0").and_then(Value::as_u64)? as u32;
            DEVICE_TYPES
                .iter()
                .find_map(|(known, giap)| (*known == id).then_some(*giap))
        })
    })
}

/// A commissioned node as reported by `start_listening` / node events.
#[derive(Debug, Clone, Deserialize)]
pub struct MatterNode {
    pub node_id: u64,
    #[serde(default)]
    pub available: bool,
    /// Flat attribute map keyed `"endpoint/cluster/attribute"`.
    #[serde(default)]
    pub attributes: BTreeMap<String, Value>,
}

/// A parsed message from the server.
#[derive(Debug)]
pub enum ServerMessage {
    Result {
        message_id: String,
        result: Value,
    },
    Error {
        message_id: String,
        details: String,
    },
    Event {
        event: String,
        data: Value,
    },
    /// The greeting / anything else we don't act on.
    Other,
}

/// Parse one raw server frame.
pub fn parse_server_message(raw: &str) -> ServerMessage {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return ServerMessage::Other;
    };
    if let Some(event) = v.get("event").and_then(Value::as_str) {
        return ServerMessage::Event {
            event: event.to_string(),
            data: v.get("data").cloned().unwrap_or(Value::Null),
        };
    }
    if let Some(mid) = v.get("message_id").and_then(Value::as_str) {
        if v.get("error_code").is_some() {
            return ServerMessage::Error {
                message_id: mid.to_string(),
                details: v
                    .get("details")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown matter-server error")
                    .to_string(),
            };
        }
        if let Some(result) = v.get("result") {
            return ServerMessage::Result {
                message_id: mid.to_string(),
                result: result.clone(),
            };
        }
    }
    ServerMessage::Other
}

/// Build a command frame.
pub fn command_frame(message_id: &str, command: &str, args: Value) -> String {
    json!({ "message_id": message_id, "command": command, "args": args }).to_string()
}

/// GIAP device id for a Matter node (`"matter-<node_id>"`).
pub fn device_id_for_node(node_id: u64) -> String {
    format!("matter-{node_id}")
}

/// Inverse of [`device_id_for_node`]; `None` for non-Matter ids.
pub fn node_id_from_device_id(device_id: &str) -> Option<u64> {
    device_id.strip_prefix("matter-")?.parse().ok()
}

/// The endpoints (ascending) on which `node` hosts `cluster` — non-root only,
/// since endpoint 0 carries utility clusters, not application ones.
pub fn endpoints_with_cluster(node: &MatterNode, cluster: u32) -> Vec<u16> {
    let mut eps: Vec<u16> = node
        .attributes
        .keys()
        .filter_map(|key| {
            let mut parts = key.split('/');
            let ep: u16 = parts.next()?.parse().ok()?;
            let cl: u32 = parts.next()?.parse().ok()?;
            (cl == cluster && ep != 0).then_some(ep)
        })
        .collect();
    eps.sort_unstable();
    eps.dedup();
    eps
}

/// Project a commissioned node onto GIAP's [`Device`]. Works for ANY Matter
/// device — real bulbs, locks, thermostats, or virtual test devices; nothing
/// here is specific to a vendor or to test tooling. Type and capabilities are
/// inferred from the application clusters present.
///
/// Naming follows what production controllers do — take the device's own
/// identity, best source first:
/// 1. Basic Information NodeLabel (`0/40/5`) — the user-assigned name;
/// 2. Basic Information ProductName (`0/40/3`) — the vendor's name
///    (e.g. "Hue color lamp");
/// 3. `"<Type> <node_id>"` (e.g. "Light 2") — a clean, speakable fallback.
pub fn node_to_device(node: &MatterNode) -> Device {
    let has = |cluster: u32| !endpoints_with_cluster(node, cluster).is_empty();

    let basic_info = |attribute: u32| {
        node.attributes
            .get(&format!("0/{CLUSTER_BASIC_INFORMATION}/{attribute}"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    let mut capabilities = Vec::new();
    if has(CLUSTER_ON_OFF) {
        capabilities.push("power".to_string());
    }
    if has(CLUSTER_FAN_CONTROL) {
        // A Matter fan need not implement On/Off at all — the Virtual Fan does
        // not — so without this it advertised no capabilities and "turn on the
        // fan" had nothing to aim at. `FanMode` is its power switch.
        if !has(CLUSTER_ON_OFF) {
            capabilities.push("power".to_string());
        }
        capabilities.push("fan_speed".to_string());
    }
    if has(CLUSTER_LEVEL_CONTROL) {
        capabilities.push("brightness".to_string());
    }
    if has(CLUSTER_THERMOSTAT) {
        capabilities.push("temperature".to_string());
    }
    if has(CLUSTER_DOOR_LOCK) {
        capabilities.push("lock".to_string());
    }

    // The node's own word first. Clusters can only say what is drivable, which
    // is why every On/Off appliance used to arrive as a light.
    let device_type = if let Some(stated) = device_type_from_descriptor(node) {
        stated
    } else if has(CLUSTER_DOOR_LOCK) {
        "lock"
    } else if has(CLUSTER_THERMOSTAT) {
        "thermostat"
    } else if has(CLUSTER_FAN_CONTROL) {
        // Ahead of the On/Off check: a fan that does implement On/Off is still
        // a fan, and calling it a light gives the model the wrong vocabulary.
        "fan"
    } else if has(CLUSTER_ON_OFF) {
        "light"
    } else if has(CLUSTER_OCCUPANCY)
        || has(CLUSTER_BOOLEAN_STATE)
        || has(CLUSTER_TEMPERATURE)
        || has(CLUSTER_HUMIDITY)
    {
        "sensor"
    } else {
        "matter"
    };

    let name = basic_info(5) // NodeLabel — user-assigned
        .or_else(|| basic_info(3)) // ProductName — vendor-assigned
        .unwrap_or_else(|| {
            // Speakable typed fallback, e.g. "Light 2".
            let mut typed = device_type.to_string();
            if let Some(first) = typed.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            format!("{typed} {}", node.node_id)
        });

    Device {
        id: device_id_for_node(node.node_id),
        name,
        device_type: device_type.to_string(),
        hostname: None,
        ip_address: None,
        capabilities,
        registered_at: Utc::now().to_rfc3339(),
        last_seen: Some(Utc::now().to_rfc3339()),
        is_online: node.available,
        room: None,
    }
}

/// Translate an `attribute_updated` event into a [`SensorReading`], when the
/// attribute belongs to a sensor cluster GIAP understands. Everything else
/// (lights confirming state, utility clusters) returns `None`.
pub fn sensor_reading_from_update(
    node_id: u64,
    path: &str,
    value: &Value,
) -> Option<SensorReading> {
    let mut parts = path.split('/');
    let _endpoint: u16 = parts.next()?.parse().ok()?;
    let cluster: u32 = parts.next()?.parse().ok()?;
    let attribute: u32 = parts.next()?.parse().ok()?;

    // Filter monitoring is the one thing here that reports on two attributes:
    // how worn the filter is, and whether the device is asking for it to be
    // changed. Both are worth knowing and they answer different questions.
    if let Some((sensor_type, reading, unit)) = match (cluster, attribute) {
        (CLUSTER_HEPA_FILTER, ATTR_FILTER_CONDITION) => {
            Some(("hepa_filter_condition", value.as_f64()?, "%"))
        }
        (CLUSTER_HEPA_FILTER, ATTR_FILTER_CHANGE_INDICATION) => {
            Some(("hepa_filter_change", value.as_u64()? as f64, "state"))
        }
        (CLUSTER_ACTIVATED_CARBON_FILTER, ATTR_FILTER_CONDITION) => {
            Some(("carbon_filter_condition", value.as_f64()?, "%"))
        }
        (CLUSTER_ACTIVATED_CARBON_FILTER, ATTR_FILTER_CHANGE_INDICATION) => {
            Some(("carbon_filter_change", value.as_u64()? as f64, "state"))
        }
        _ => None,
    } {
        return Some(SensorReading {
            device_id: device_id_for_node(node_id),
            sensor_type: sensor_type.to_string(),
            value: reading,
            unit: unit.to_string(),
            recorded_at: Utc::now(),
        });
    }

    if attribute != 0 {
        return None; // every other measurement cluster reports on attribute 0
    }

    let (sensor_type, reading, unit) = match cluster {
        // Occupancy bitmap: bit 0 = occupied.
        CLUSTER_OCCUPANCY => ("occupancy", ((value.as_u64()? & 1) as f64), "bool"),
        // BooleanState: contact sensors (true = closed per Matter).
        CLUSTER_BOOLEAN_STATE => ("contact", f64::from(value.as_bool()?), "bool"),
        // Hundredths of a degree Celsius.
        CLUSTER_TEMPERATURE => ("temperature", value.as_i64()? as f64 / 100.0, "C"),
        // Hundredths of a percent.
        CLUSTER_HUMIDITY => ("humidity", value.as_i64()? as f64 / 100.0, "%"),
        // Lux, reported as a log-scaled value; the raw measurement is what a
        // rule threshold compares, so it is passed through unconverted.
        CLUSTER_ILLUMINANCE => ("illuminance", value.as_i64()? as f64, "lux"),
        // Tenths of a kPa.
        CLUSTER_PRESSURE => ("pressure", value.as_i64()? as f64 / 10.0, "kPa"),
        // Tenths of a cubic metre per hour.
        CLUSTER_FLOW => ("flow", value.as_i64()? as f64 / 10.0, "m3/h"),
        // An ordinal: 0 unknown, 1 good, rising to 6 extremely poor. Kept as
        // the ordinal rather than invented units, so the scale stays the
        // device's own.
        CLUSTER_AIR_QUALITY => ("air_quality", value.as_u64()? as f64, "level"),
        // Alarm state: 0 normal, non-zero means it is sounding.
        CLUSTER_SMOKE_CO_ALARM => ("smoke_alarm", value.as_u64()? as f64, "state"),
        // Concentrations are floats in the substance's own unit, passed
        // through unscaled — the number the device shows is the number a rule
        // threshold should compare against.
        CLUSTER_CO => ("carbon_monoxide", value.as_f64()?, "ppm"),
        CLUSTER_CO2 => ("carbon_dioxide", value.as_f64()?, "ppm"),
        CLUSTER_NO2 => ("nitrogen_dioxide", value.as_f64()?, "ppb"),
        CLUSTER_OZONE => ("ozone", value.as_f64()?, "ppb"),
        CLUSTER_FORMALDEHYDE => ("formaldehyde", value.as_f64()?, "mg/m3"),
        CLUSTER_PM1 => ("pm1", value.as_f64()?, "ug/m3"),
        CLUSTER_PM25 => ("pm2_5", value.as_f64()?, "ug/m3"),
        CLUSTER_PM10 => ("pm10", value.as_f64()?, "ug/m3"),
        CLUSTER_RADON => ("radon", value.as_f64()?, "ppm"),
        CLUSTER_TVOC => ("total_volatile_organic_compounds", value.as_f64()?, "ppb"),
        _ => return None,
    };

    Some(SensorReading {
        device_id: device_id_for_node(node_id),
        sensor_type: sensor_type.to_string(),
        value: reading,
        unit: unit.to_string(),
        recorded_at: Utc::now(),
    })
}

/// Map a 0–100 GIAP brightness percentage onto Matter's 0–254 level scale.
pub fn brightness_to_level(percent: u8) -> u8 {
    ((u16::from(percent.min(100)) * 254 + 50) / 100) as u8
}

/// Celsius → Matter thermostat setpoint (hundredths of a degree).
pub fn celsius_to_setpoint(celsius: f32) -> i16 {
    (celsius * 100.0)
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

/// Map a 0–360° hue onto Matter ColorControl's 0–254 hue scale (360° wraps to
/// 0, matching the circular hue space).
pub fn hue_to_matter(degrees: u16) -> u8 {
    ((u32::from(degrees % 360) * 254 + 180) / 360) as u8
}

/// Map a 0–100 saturation percentage onto Matter's 0–254 saturation scale.
pub fn saturation_to_matter(percent: u8) -> u8 {
    ((u16::from(percent.min(100)) * 254 + 50) / 100) as u8
}

/// Map a GIAP covering position (0–100 percent **open**) onto Matter
/// WindowCovering's lift value in hundredths-of-a-percent **closed**
/// (`GoToLiftPercentage`): 0 = fully open, 10000 = fully closed. GIAP speaks in
/// "percent open" because that is how users phrase it ("open the blinds 50%").
pub fn position_open_to_lift_100ths(percent_open: u8) -> u16 {
    u16::from(100 - percent_open.min(100)) * 100
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Matter Virtual Device's fan, as commissioned on 2026-08-05: Fan
    /// Control on endpoint 1 and **no On/Off cluster at all**, which is what
    /// left it with no capabilities and unreachable by "turn on the fan".
    fn fan_node() -> MatterNode {
        serde_json::from_value(json!({
            "node_id": 18,
            "available": true,
            "attributes": {
                "0/40/5": "Living Room Fan",
                "1/514/0": 0,
                "1/514/2": 0,
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_fan_without_on_off_is_still_powerable_and_typed_as_a_fan() {
        let device = node_to_device(&fan_node());
        assert_eq!(device.device_type, "fan", "not a light, and not untyped");
        // Power comes from FanMode here; fan_speed from PercentSetting.
        assert_eq!(device.capabilities, vec!["power", "fan_speed"]);
        assert_eq!(
            endpoints_with_cluster(&fan_node(), CLUSTER_FAN_CONTROL),
            vec![1]
        );
        assert!(endpoints_with_cluster(&fan_node(), CLUSTER_ON_OFF).is_empty());
    }

    #[test]
    fn a_fan_that_does_implement_on_off_reports_power_once() {
        let mut node = fan_node();
        node.attributes.insert("1/6/0".to_string(), json!(false));

        let device = node_to_device(&node);
        assert_eq!(device.device_type, "fan", "a fan with a switch is a fan");
        assert_eq!(
            device.capabilities,
            vec!["power", "fan_speed"],
            "power must not be listed twice when both clusters are present"
        );
    }

    /// A node that states its type the way every real one does: Descriptor
    /// (cluster 29) attribute 0 on the application endpoint. `device` is the
    /// Matter device type id.
    fn described_node(node_id: u64, device: u32, extra: &[(&str, Value)]) -> MatterNode {
        let mut attributes = json!({
            "0/29/0": [{ "0": 22, "1": 1 }],
            "1/29/0": [{ "0": device, "1": 1 }],
        });
        for (path, value) in extra {
            attributes[path] = value.clone();
        }
        serde_json::from_value(json!({
            "node_id": node_id,
            "available": true,
            "attributes": attributes,
        }))
        .unwrap()
    }

    /// The regression that started this: a plug and a bulb are both On/Off, so
    /// cluster inference called every plug a light and the UI drew a lightbulb
    /// on it. The node says which it is.
    #[test]
    fn a_plug_is_a_plug_even_though_it_looks_like_a_light() {
        let plug = described_node(30, 0x010A, &[("1/6/0", json!(false))]);
        assert_eq!(node_to_device(&plug).device_type, "plug");

        // And the bulb it was indistinguishable from is still a light.
        let bulb = described_node(31, 0x0100, &[("1/6/0", json!(false))]);
        assert_eq!(node_to_device(&bulb).device_type, "light");
    }

    /// The types that used to land as the generic "matter" with a Monitor icon.
    #[test]
    fn appliances_alarms_and_coverings_get_their_own_types() {
        for (id, want) in [
            (0x0075u32, "appliance"), // Dishwasher
            (0x0073, "appliance"),    // Laundry Washer
            (0x0303, "pump"),         // Pump
            (0x0028, "media"),        // Basic Video Player
            (0x0074, "vacuum"),       // Robotic Vacuum
            (0x0076, "alarm"),        // Smoke/CO Alarm
            (0x0202, "covering"),     // Window Covering
            (0x002C, "air"),          // Air Purifier
            (0x002D, "sensor"),       // Air Quality Sensor
        ] {
            let node = described_node(40, id, &[]);
            assert_eq!(
                node_to_device(&node).device_type,
                want,
                "device type 0x{id:04X}"
            );
        }
    }

    /// Endpoint 0 is the Root Node on every device. Reading it would type the
    /// whole fabric as one thing.
    #[test]
    fn the_root_endpoint_is_not_mistaken_for_the_device() {
        let node = described_node(41, 0x0075, &[]);
        assert_eq!(device_type_from_descriptor(&node), Some("appliance"));

        // A node with only the root endpoint states nothing about itself.
        let root_only: MatterNode = serde_json::from_value(json!({
            "node_id": 42,
            "available": true,
            "attributes": { "0/29/0": [{ "0": 22, "1": 1 }] },
        }))
        .unwrap();
        assert_eq!(device_type_from_descriptor(&root_only), None);
    }

    /// A node with no readable descriptor — an older device, or one whose
    /// descriptor GIAP does not recognise — must behave exactly as before.
    #[test]
    fn without_a_descriptor_the_cluster_inference_still_decides() {
        assert_eq!(device_type_from_descriptor(&light_node()), None);
        assert_eq!(node_to_device(&light_node()).device_type, "light");

        // An unknown device type id falls through to the clusters too.
        let unknown = described_node(43, 0xBEEF, &[("1/6/0", json!(false))]);
        assert_eq!(node_to_device(&unknown).device_type, "light");
    }

    /// Every mode the Virtual Air Purifier offers, by the name a user says.
    #[test]
    fn fan_modes_map_from_the_names_a_user_uses() {
        for (name, code) in [
            ("off", 0u8),
            ("low", 1),
            ("medium", 2),
            ("high", 3),
            ("on", 4),
            ("auto", 5),
            ("smart", 6),
        ] {
            assert_eq!(fan_mode_from_name(name), Some(code), "{name}");
        }

        // Spoken input is not tidy.
        assert_eq!(fan_mode_from_name("  HIGH "), Some(3));
        assert_eq!(fan_mode_from_name("Med"), Some(2));
        // And an invented mode is refused rather than guessed at.
        assert_eq!(fan_mode_from_name("turbo"), None);
        assert_eq!(fan_mode_from_name(""), None);
    }

    /// The Air Quality Sensor's substances. Each is its own cluster reporting
    /// MeasuredValue on attribute 0, and each needs its own name or they
    /// collapse into one unreadable "air quality" number.
    #[test]
    fn each_measured_substance_reports_under_its_own_name() {
        let cases = [
            (CLUSTER_CO, "carbon_monoxide", "ppm"),
            (CLUSTER_CO2, "carbon_dioxide", "ppm"),
            (CLUSTER_NO2, "nitrogen_dioxide", "ppb"),
            (CLUSTER_OZONE, "ozone", "ppb"),
            (CLUSTER_FORMALDEHYDE, "formaldehyde", "mg/m3"),
            (CLUSTER_PM1, "pm1", "ug/m3"),
            (CLUSTER_PM25, "pm2_5", "ug/m3"),
            (CLUSTER_PM10, "pm10", "ug/m3"),
            (CLUSTER_RADON, "radon", "ppm"),
            (CLUSTER_TVOC, "total_volatile_organic_compounds", "ppb"),
        ];
        for (cluster, name, unit) in cases {
            let reading = sensor_reading_from_update(5, &format!("1/{cluster}/0"), &json!(636.0))
                .unwrap_or_else(|| panic!("cluster {cluster} should report"));
            assert_eq!(reading.sensor_type, name);
            assert_eq!(reading.unit, unit);
            assert!((reading.value - 636.0).abs() < f64::EPSILON);
        }
    }

    /// The substance defaults are a guess; the device publishes the truth on a
    /// sibling attribute. A CO2 sensor reporting ppb was labelled ppm — fine on
    /// screen, wrong the moment a rule or a summary compares the number.
    #[test]
    fn a_declared_measurement_unit_overrides_the_substance_default() {
        let node: MatterNode = serde_json::from_value(json!({
            "node_id": 7,
            "available": true,
            "attributes": {
                // CO2 measured in ppb (unit code 1), not the ppm we assume.
                format!("1/{CLUSTER_CO2}/0"): 636.0,
                format!("1/{CLUSTER_CO2}/8"): 1,
                // PM2.5 with no declared unit at all.
                format!("1/{CLUSTER_PM25}/0"): 12.0,
            }
        }))
        .unwrap();

        assert_eq!(
            declared_unit_for(&node, &format!("1/{CLUSTER_CO2}/0")),
            Some("ppb")
        );
        // Nothing declared: the caller keeps the substance default.
        assert_eq!(
            declared_unit_for(&node, &format!("1/{CLUSTER_PM25}/0")),
            None
        );
        // Not a concentration cluster, so the question does not apply.
        assert_eq!(
            declared_unit_for(&node, &format!("1/{CLUSTER_TEMPERATURE}/0")),
            None
        );
    }

    #[test]
    fn an_unknown_unit_code_is_not_guessed_at() {
        let node: MatterNode = serde_json::from_value(json!({
            "node_id": 8,
            "available": true,
            "attributes": { format!("1/{CLUSTER_OZONE}/8"): 99 }
        }))
        .unwrap();
        assert_eq!(
            declared_unit_for(&node, &format!("1/{CLUSTER_OZONE}/0")),
            None
        );
    }

    /// Filter monitoring is the one cluster here reporting on two attributes:
    /// how worn the filter is, and whether the device is asking for a change.
    /// They answer different questions and must not be collapsed.
    #[test]
    fn both_filters_report_condition_and_change_indication() {
        let hepa_condition =
            sensor_reading_from_update(6, &format!("1/{CLUSTER_HEPA_FILTER}/0"), &json!(100.0))
                .unwrap();
        assert_eq!(hepa_condition.sensor_type, "hepa_filter_condition");
        assert_eq!(hepa_condition.unit, "%");
        assert!((hepa_condition.value - 100.0).abs() < f64::EPSILON);

        // ChangeIndication lives on attribute 2, which the attribute-0 rule
        // for every other measurement cluster would otherwise discard.
        let hepa_change =
            sensor_reading_from_update(6, &format!("1/{CLUSTER_HEPA_FILTER}/2"), &json!(2))
                .unwrap();
        assert_eq!(hepa_change.sensor_type, "hepa_filter_change");
        assert_eq!(hepa_change.value, 2.0, "2 = Critical");

        let carbon = sensor_reading_from_update(
            6,
            &format!("1/{CLUSTER_ACTIVATED_CARBON_FILTER}/0"),
            &json!(45.0),
        )
        .unwrap();
        assert_eq!(carbon.sensor_type, "carbon_filter_condition");

        // The two filters stay distinguishable — one device has both.
        assert_ne!(hepa_condition.sensor_type, carbon.sensor_type);
    }

    #[test]
    fn the_added_sensor_clusters_produce_readings_with_their_units() {
        let cases = [
            (
                CLUSTER_ILLUMINANCE,
                json!(1200),
                "illuminance",
                1200.0,
                "lux",
            ),
            (CLUSTER_PRESSURE, json!(1013), "pressure", 101.3, "kPa"),
            (CLUSTER_FLOW, json!(25), "flow", 2.5, "m3/h"),
            (CLUSTER_AIR_QUALITY, json!(3), "air_quality", 3.0, "level"),
            (
                CLUSTER_SMOKE_CO_ALARM,
                json!(1),
                "smoke_alarm",
                1.0,
                "state",
            ),
        ];
        for (cluster, raw, kind, value, unit) in cases {
            let reading = sensor_reading_from_update(9, &format!("1/{cluster}/0"), &raw)
                .unwrap_or_else(|| panic!("cluster {cluster} should report"));
            assert_eq!(reading.sensor_type, kind);
            assert!((reading.value - value).abs() < f64::EPSILON, "{kind}");
            assert_eq!(reading.unit, unit);
            assert_eq!(reading.device_id, "matter-9");
        }
    }

    fn light_node() -> MatterNode {
        serde_json::from_value(json!({
            "node_id": 2,
            "available": true,
            "attributes": {
                "0/40/5": "Living Room Light",
                "0/40/1": "TEST_VENDOR",
                "13/6/0": false,
                "13/8/0": 1,
            }
        }))
        .unwrap()
    }

    #[test]
    fn parses_results_errors_and_events() {
        match parse_server_message(r#"{"message_id":"1","result":{"ok":true}}"#) {
            ServerMessage::Result { message_id, .. } => assert_eq!(message_id, "1"),
            other => panic!("expected result, got {other:?}"),
        }
        match parse_server_message(r#"{"message_id":"2","error_code":1,"details":"boom"}"#) {
            ServerMessage::Error { details, .. } => assert_eq!(details, "boom"),
            other => panic!("expected error, got {other:?}"),
        }
        match parse_server_message(r#"{"event":"attribute_updated","data":[2,"13/6/0",true]}"#) {
            ServerMessage::Event { event, data } => {
                assert_eq!(event, "attribute_updated");
                assert_eq!(data[1], "13/6/0");
            }
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn maps_the_live_session_light_to_a_giap_device() {
        // Cluster layout mirrors the node commissioned in the live session
        // (OnOff + LevelControl on endpoint 13) — identical for real bulbs.
        let device = node_to_device(&light_node());
        assert_eq!(device.id, "matter-2");
        assert_eq!(device.name, "Living Room Light");
        assert_eq!(device.device_type, "light");
        assert_eq!(device.capabilities, vec!["power", "brightness"]);
        assert!(device.is_online);
        assert_eq!(
            endpoints_with_cluster(&light_node(), CLUSTER_ON_OFF),
            vec![13]
        );
    }

    /// Production naming chain: user label first, then the vendor's product
    /// name (what a real bulb reports), then a speakable typed fallback —
    /// never a raw protocol identifier.
    #[test]
    fn naming_falls_back_from_label_to_product_name_to_type() {
        let mut node = light_node();
        // No NodeLabel -> vendor ProductName (what real bulbs carry).
        node.attributes.remove("0/40/5");
        node.attributes
            .insert("0/40/3".into(), json!("Hue color lamp"));
        assert_eq!(node_to_device(&node).name, "Hue color lamp");

        // Neither -> "Light 2", not "Matter node 2".
        node.attributes.remove("0/40/3");
        assert_eq!(node_to_device(&node).name, "Light 2");

        // Blank labels are treated as absent, not used verbatim.
        node.attributes.insert("0/40/5".into(), json!("  "));
        assert_eq!(node_to_device(&node).name, "Light 2");
    }

    #[test]
    fn device_id_round_trips_and_rejects_foreign_ids() {
        assert_eq!(device_id_for_node(2), "matter-2");
        assert_eq!(node_id_from_device_id("matter-2"), Some(2));
        assert_eq!(node_id_from_device_id("living-room-light"), None);
        assert_eq!(node_id_from_device_id("matter-abc"), None);
    }

    #[test]
    fn sensor_updates_translate_and_actuator_updates_do_not() {
        let occ = sensor_reading_from_update(7, "1/1030/0", &json!(1)).unwrap();
        assert_eq!(
            (occ.device_id.as_str(), occ.sensor_type.as_str()),
            ("matter-7", "occupancy")
        );
        assert_eq!(occ.value, 1.0);

        let temp = sensor_reading_from_update(8, "1/1026/0", &json!(2150)).unwrap();
        assert_eq!(temp.sensor_type, "temperature");
        assert!((temp.value - 21.5).abs() < 1e-9);
        assert_eq!(temp.unit, "C");

        let contact = sensor_reading_from_update(9, "1/69/0", &json!(false)).unwrap();
        assert_eq!(
            (contact.sensor_type.as_str(), contact.value),
            ("contact", 0.0)
        );

        // A light confirming its OnOff state is NOT a sensor reading.
        assert!(sensor_reading_from_update(2, "13/6/0", &json!(true)).is_none());
        // Non-zero attributes of sensor clusters are ignored too.
        assert!(sensor_reading_from_update(7, "1/1030/1", &json!(3)).is_none());
    }

    #[test]
    fn unit_conversions_hit_matter_scales() {
        assert_eq!(brightness_to_level(0), 0);
        assert_eq!(brightness_to_level(100), 254);
        assert_eq!(brightness_to_level(50), 127);
        assert_eq!(brightness_to_level(200), 254); // clamped
        assert_eq!(celsius_to_setpoint(21.5), 2150);
        assert_eq!(celsius_to_setpoint(-5.25), -525);
    }

    #[test]
    fn color_fan_covering_conversions_hit_matter_scales() {
        // Hue: 0–360° onto 0–254, with 360° wrapping back to 0.
        assert_eq!(hue_to_matter(0), 0);
        assert_eq!(hue_to_matter(360), 0);
        assert_eq!(hue_to_matter(180), 127);
        assert_eq!(hue_to_matter(720), 0); // wraps

        // Saturation: 0–100% onto 0–254.
        assert_eq!(saturation_to_matter(0), 0);
        assert_eq!(saturation_to_matter(100), 254);
        assert_eq!(saturation_to_matter(200), 254); // clamped

        // Covering: percent-open inverted to Matter's lift (100ths closed).
        assert_eq!(position_open_to_lift_100ths(100), 0); // fully open
        assert_eq!(position_open_to_lift_100ths(0), 10000); // fully closed
        assert_eq!(position_open_to_lift_100ths(50), 5000);
        assert_eq!(position_open_to_lift_100ths(200), 0); // clamped open
    }
}
