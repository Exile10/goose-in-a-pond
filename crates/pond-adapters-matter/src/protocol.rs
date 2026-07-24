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
pub const CLUSTER_BOOLEAN_STATE: u32 = 69;
pub const CLUSTER_DOOR_LOCK: u32 = 257;
pub const CLUSTER_THERMOSTAT: u32 = 513;
pub const CLUSTER_TEMPERATURE: u32 = 1026;
pub const CLUSTER_HUMIDITY: u32 = 1029;
pub const CLUSTER_OCCUPANCY: u32 = 1030;

/// Thermostat `OccupiedHeatingSetpoint` attribute id.
pub const ATTR_OCCUPIED_HEATING_SETPOINT: u32 = 18;

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
    if has(CLUSTER_LEVEL_CONTROL) {
        capabilities.push("brightness".to_string());
    }
    if has(CLUSTER_THERMOSTAT) {
        capabilities.push("temperature".to_string());
    }
    if has(CLUSTER_DOOR_LOCK) {
        capabilities.push("lock".to_string());
    }

    let device_type = if has(CLUSTER_DOOR_LOCK) {
        "lock"
    } else if has(CLUSTER_THERMOSTAT) {
        "thermostat"
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
    if attribute != 0 {
        return None; // measurement clusters report on attribute 0
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
