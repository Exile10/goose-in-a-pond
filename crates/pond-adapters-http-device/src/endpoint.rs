use std::collections::HashMap;

use pond_core::domain::device::{DeviceState, StateValue};

/// A single outgoing HTTP request derived from a device operation.
#[derive(Debug)]
pub enum HttpRequest {
    Get { url: String },
    Post { url: String, body: Option<serde_json::Value> },
}

/// Encodes URL/body differences between HTTP smart-device protocols.
///
/// The `address` field in the device registry stores the base URL for the device.
/// For ESPHome, it should include the component path
/// (e.g. `http://192.168.1.42/switch/my_switch`).
/// For Shelly and Generic, it is the device root (e.g. `http://192.168.1.42`).
pub enum EndpointScheme {
    /// Shelly Gen1 HTTP API.
    ///
    /// Power:  `GET {base}/relay/0?turn=on|off`
    /// State:  `GET {base}/relay/0`
    /// Set:    `GET {base}/relay/0?{key}={value}`
    Shelly { base: String },

    /// ESPHome native REST API. `base` must be the full component URL,
    /// e.g. `http://host/switch/my_switch` or `http://host/light/ceiling`.
    ///
    /// Power:  `POST {base}/turn_on|off`
    /// State:  `GET  {base}`
    /// Set:    `POST {base}/set` with JSON body `{"value": v}`
    EspHome { base: String },

    /// Generic REST device. `base` is the device root URL.
    ///
    /// Power:  `POST {base}` with JSON body `{"on": true|false}`
    /// State:  `GET  {base}`
    /// Set:    `POST {base}` with JSON body `{key: value}`
    Generic { base: String },
}

impl EndpointScheme {
    /// Derive the scheme from `device_type` and the base address stored in the registry.
    pub fn from_device_type(device_type: &str, base: &str) -> Self {
        match device_type {
            "shelly" => EndpointScheme::Shelly { base: base.to_string() },
            "esphome" => EndpointScheme::EspHome { base: base.to_string() },
            _ => EndpointScheme::Generic { base: base.to_string() },
        }
    }

    /// Request to toggle device power.
    pub fn power_request(&self, on: bool) -> HttpRequest {
        let onoff = if on { "on" } else { "off" };
        match self {
            EndpointScheme::Shelly { base } => {
                HttpRequest::Get { url: format!("{base}/relay/0?turn={onoff}") }
            }
            EndpointScheme::EspHome { base } => {
                HttpRequest::Post { url: format!("{base}/turn_{onoff}"), body: None }
            }
            EndpointScheme::Generic { base } => HttpRequest::Post {
                url: base.clone(),
                body: Some(serde_json::json!({ "on": on })),
            },
        }
    }

    /// Request to read current device state.
    pub fn state_request(&self) -> HttpRequest {
        match self {
            EndpointScheme::Shelly { base } => {
                HttpRequest::Get { url: format!("{base}/relay/0") }
            }
            EndpointScheme::EspHome { base } => HttpRequest::Get { url: base.clone() },
            EndpointScheme::Generic { base } => HttpRequest::Get { url: base.clone() },
        }
    }

    /// Request to write an arbitrary state key.
    pub fn set_state_request(&self, key: &str, value: &serde_json::Value) -> HttpRequest {
        match self {
            EndpointScheme::Shelly { base } => {
                // Shelly accepts query-param overrides for simple scalar values.
                let v = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                HttpRequest::Get { url: format!("{base}/relay/0?{key}={v}") }
            }
            EndpointScheme::EspHome { base } => HttpRequest::Post {
                url: format!("{base}/set"),
                body: Some(serde_json::json!({ "value": value })),
            },
            EndpointScheme::Generic { base } => HttpRequest::Post {
                url: base.clone(),
                body: Some(serde_json::json!({ key: value })),
            },
        }
    }

    /// Parse a JSON response body into a `DeviceState`.
    ///
    /// Normalises protocol-specific field names so callers always see `"power": Bool`.
    pub fn parse_state(&self, body: &str, device_id: &str) -> DeviceState {
        let mut values: HashMap<String, StateValue> = HashMap::new();

        let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
            return DeviceState { device_id: device_id.to_string(), values };
        };

        let Some(obj) = json.as_object() else {
            return DeviceState { device_id: device_id.to_string(), values };
        };

        // Flatten top-level scalar fields into the state map.
        for (k, v) in obj {
            if let Some(sv) = json_to_state_value(v) {
                values.insert(k.clone(), sv);
            }
        }

        // Protocol-specific normalisations → canonical `"power"` key.
        match self {
            EndpointScheme::Shelly { .. } => {
                // Shelly: `ison` → `power`
                if let Some(sv) = values.get("ison").cloned() {
                    values.insert("power".to_string(), sv);
                }
            }
            EndpointScheme::EspHome { .. } => {
                // ESPHome `state` is already Bool after json_to_state_value converts
                // "ON"/"OFF" strings. Copy it to the canonical `power` key.
                if let Some(sv) = values.get("state").cloned() {
                    values.insert("power".to_string(), sv);
                }
            }
            EndpointScheme::Generic { .. } => {
                // Generic: honour `on`/`power` if present.
                if let Some(sv) = values.get("on").cloned() {
                    values.insert("power".to_string(), sv);
                }
            }
        }

        DeviceState { device_id: device_id.to_string(), values }
    }
}

fn json_to_state_value(v: &serde_json::Value) -> Option<StateValue> {
    match v {
        serde_json::Value::Bool(b) => Some(StateValue::Bool(*b)),
        serde_json::Value::Number(n) => Some(StateValue::Number(n.as_f64()?)),
        serde_json::Value::String(s) => match s.as_str() {
            "ON" | "on" => Some(StateValue::Bool(true)),
            "OFF" | "off" => Some(StateValue::Bool(false)),
            _ => Some(StateValue::Text(s.clone())),
        },
        // Nested objects / arrays are skipped — callers use structured_capabilities
        // to know what to expect; we only expose scalars here.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── power_request ──────────────────────────────────────────────────────────

    #[test]
    fn shelly_power_on_is_get_with_turn_on() {
        let scheme = EndpointScheme::from_device_type("shelly", "http://192.168.1.10");
        let req = scheme.power_request(true);
        match req {
            HttpRequest::Get { url } => assert_eq!(url, "http://192.168.1.10/relay/0?turn=on"),
            _ => panic!("expected GET"),
        }
    }

    #[test]
    fn shelly_power_off_is_get_with_turn_off() {
        let scheme = EndpointScheme::from_device_type("shelly", "http://192.168.1.10");
        let req = scheme.power_request(false);
        match req {
            HttpRequest::Get { url } => assert_eq!(url, "http://192.168.1.10/relay/0?turn=off"),
            _ => panic!("expected GET"),
        }
    }

    #[test]
    fn esphome_power_on_is_post_to_turn_on() {
        let scheme = EndpointScheme::from_device_type(
            "esphome",
            "http://192.168.1.20/switch/my_switch",
        );
        let req = scheme.power_request(true);
        match req {
            HttpRequest::Post { url, body: None } => {
                assert_eq!(url, "http://192.168.1.20/switch/my_switch/turn_on");
            }
            _ => panic!("expected bodyless POST"),
        }
    }

    #[test]
    fn generic_power_off_is_post_with_json_body() {
        let scheme = EndpointScheme::from_device_type("http", "http://192.168.1.30/plug");
        let req = scheme.power_request(false);
        match req {
            HttpRequest::Post { url, body: Some(b) } => {
                assert_eq!(url, "http://192.168.1.30/plug");
                assert_eq!(b["on"], serde_json::json!(false));
            }
            _ => panic!("expected POST with body"),
        }
    }

    // ── parse_state ───────────────────────────────────────────────────────────

    #[test]
    fn shelly_parse_ison_normalised_to_power() {
        let scheme = EndpointScheme::from_device_type("shelly", "http://host");
        let body = r#"{"ison":true,"has_timer":false,"timer_remaining":0}"#;
        let state = scheme.parse_state(body, "dev-1");
        assert_eq!(state.values["ison"], StateValue::Bool(true));
        assert_eq!(state.values["power"], StateValue::Bool(true));
    }

    #[test]
    fn esphome_parse_state_on_normalised() {
        let scheme = EndpointScheme::from_device_type("esphome", "http://host/switch/s");
        let body = r#"{"id":"switch-s","state":"ON","value":1.0}"#;
        let state = scheme.parse_state(body, "dev-2");
        assert_eq!(state.values["power"], StateValue::Bool(true));
        assert_eq!(state.values["value"], StateValue::Number(1.0));
    }

    #[test]
    fn generic_parse_on_field_normalised_to_power() {
        let scheme = EndpointScheme::from_device_type("http", "http://host/api");
        let body = r#"{"on":false,"temp":22.5}"#;
        let state = scheme.parse_state(body, "dev-3");
        assert_eq!(state.values["power"], StateValue::Bool(false));
        assert_eq!(state.values["temp"], StateValue::Number(22.5));
    }

    #[test]
    fn invalid_json_returns_empty_state() {
        let scheme = EndpointScheme::from_device_type("http", "http://host");
        let state = scheme.parse_state("not json", "dev-x");
        assert!(state.values.is_empty());
    }

    // ── set_state_request ─────────────────────────────────────────────────────

    #[test]
    fn shelly_set_brightness_is_get_with_query() {
        let scheme = EndpointScheme::from_device_type("shelly", "http://192.168.1.10");
        let req = scheme.set_state_request("brightness", &serde_json::json!(80));
        match req {
            HttpRequest::Get { url } => {
                assert_eq!(url, "http://192.168.1.10/relay/0?brightness=80");
            }
            _ => panic!("expected GET"),
        }
    }

    #[test]
    fn esphome_set_value_posts_to_set_endpoint() {
        let scheme =
            EndpointScheme::from_device_type("esphome", "http://host/number/brightness");
        let req = scheme.set_state_request("value", &serde_json::json!(75));
        match req {
            HttpRequest::Post { url, body: Some(b) } => {
                assert_eq!(url, "http://host/number/brightness/set");
                assert_eq!(b["value"], serde_json::json!(75));
            }
            _ => panic!("expected POST with body"),
        }
    }
}
