/// Encodes publish/subscribe topic differences between zigbee2mqtt, Tasmota, and generic bridges.
pub enum TopicScheme {
    /// address = "zigbee2mqtt/<device_name>".
    /// Control: publish JSON to "{address}/set".
    /// State:   subscribe "{address}" for JSON state updates.
    Zigbee2Mqtt { base: String },
    /// address = "tasmota/<device_name>" or "cmnd/<device_name>".
    /// Control: publish to "cmnd/<name>/POWER" (power), "cmnd/<name>/<Cmd>" (other keys).
    /// State:   subscribe "stat/<name>/STATE" and "stat/<name>/RESULT".
    Tasmota { name: String },
    /// Fallback: publish JSON to "{address}/set", subscribe "{address}".
    Generic { base: String },
}

impl TopicScheme {
    pub fn from_address(address: &str) -> Self {
        if address.starts_with("zigbee2mqtt/") {
            TopicScheme::Zigbee2Mqtt { base: address.to_string() }
        } else if let Some(name) = address.strip_prefix("tasmota/") {
            TopicScheme::Tasmota { name: name.to_string() }
        } else if let Some(name) = address.strip_prefix("cmnd/") {
            TopicScheme::Tasmota { name: name.to_string() }
        } else {
            TopicScheme::Generic { base: address.to_string() }
        }
    }

    /// Canonical state-cache key for this address (used to align publish-time and subscribe-time keys).
    pub fn cache_key(address: &str) -> String {
        if let Some(name) = address.strip_prefix("cmnd/") {
            format!("tasmota/{name}")
        } else {
            address.to_string()
        }
    }

    /// (topic, payload) to publish for a power toggle.
    pub fn power_set(&self, on: bool) -> (String, String) {
        let onoff = if on { "ON" } else { "OFF" };
        match self {
            TopicScheme::Zigbee2Mqtt { base } => (
                format!("{base}/set"),
                format!(r#"{{"state":"{onoff}"}}"#),
            ),
            TopicScheme::Tasmota { name } => (format!("cmnd/{name}/POWER"), onoff.to_string()),
            TopicScheme::Generic { base } => (
                format!("{base}/set"),
                format!(r#"{{"power":{}}}"#, on),
            ),
        }
    }

    /// (topic, payload) to publish for an arbitrary state key.
    pub fn state_set(&self, key: &str, value: &serde_json::Value) -> (String, String) {
        match self {
            TopicScheme::Zigbee2Mqtt { base } => {
                let payload = serde_json::json!({ key: value }).to_string();
                (format!("{base}/set"), payload)
            }
            TopicScheme::Tasmota { name } => {
                let cmd = tasmota_command_for_key(key);
                // Strip JSON string quotes from scalar values
                let raw = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (format!("cmnd/{name}/{cmd}"), raw)
            }
            TopicScheme::Generic { base } => {
                let payload = serde_json::json!({ key: value }).to_string();
                (format!("{base}/set"), payload)
            }
        }
    }
}

fn tasmota_command_for_key(key: &str) -> &str {
    match key {
        "brightness" => "Dimmer",
        "color_temp" => "CT",
        "mode" => "Mode",
        "speed" => "Speed",
        "color" => "Color",
        _ => key,
    }
}

/// For a Tasmota "stat/<name>/STATE" or "stat/<name>/RESULT" topic, return the cache key.
pub fn tasmota_cache_key(topic: &str) -> Option<String> {
    // e.g. "stat/plug1/STATE" → "tasmota/plug1"
    let mut parts = topic.splitn(3, '/');
    let prefix = parts.next()?;
    let name = parts.next()?;
    let _suffix = parts.next()?;
    if prefix == "stat" {
        Some(format!("tasmota/{name}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigbee2mqtt_power_set_publishes_to_set_topic() {
        let scheme = TopicScheme::from_address("zigbee2mqtt/lamp1");
        let (topic, payload) = scheme.power_set(true);
        assert_eq!(topic, "zigbee2mqtt/lamp1/set");
        assert!(payload.contains("ON"));
    }

    #[test]
    fn tasmota_address_publishes_to_cmnd() {
        let scheme = TopicScheme::from_address("tasmota/plug1");
        let (topic, payload) = scheme.power_set(false);
        assert_eq!(topic, "cmnd/plug1/POWER");
        assert_eq!(payload, "OFF");
    }

    #[test]
    fn cmnd_address_normalised_to_tasmota() {
        assert_eq!(TopicScheme::cache_key("cmnd/plug1"), "tasmota/plug1");
        assert_eq!(TopicScheme::cache_key("tasmota/plug1"), "tasmota/plug1");
        assert_eq!(TopicScheme::cache_key("zigbee2mqtt/lamp1"), "zigbee2mqtt/lamp1");
    }

    #[test]
    fn tasmota_cache_key_extracted_from_stat_topic() {
        assert_eq!(
            tasmota_cache_key("stat/plug1/STATE"),
            Some("tasmota/plug1".to_string())
        );
        assert_eq!(
            tasmota_cache_key("stat/my_device/RESULT"),
            Some("tasmota/my_device".to_string())
        );
        assert_eq!(tasmota_cache_key("zigbee2mqtt/lamp"), None);
    }

    #[test]
    fn zigbee2mqtt_brightness_set_encodes_json() {
        let scheme = TopicScheme::from_address("zigbee2mqtt/lamp1");
        let (topic, payload) = scheme.state_set("brightness", &serde_json::json!(200));
        assert_eq!(topic, "zigbee2mqtt/lamp1/set");
        assert_eq!(payload, r#"{"brightness":200}"#);
    }

    #[test]
    fn tasmota_brightness_maps_to_dimmer() {
        let scheme = TopicScheme::from_address("tasmota/bulb1");
        let (topic, payload) = scheme.state_set("brightness", &serde_json::json!(75));
        assert_eq!(topic, "cmnd/bulb1/Dimmer");
        assert_eq!(payload, "75");
    }
}
