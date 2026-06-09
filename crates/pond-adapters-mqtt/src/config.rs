/// Connection parameters for the MQTT broker.
#[derive(Debug, Clone)]
pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Client ID sent to the broker. Defaults to a random giap-<uuid> string.
    pub client_id: String,
    pub keep_alive_secs: u64,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 1883,
            username: None,
            password: None,
            client_id: format!("giap-{}", uuid::Uuid::new_v4()),
            keep_alive_secs: 60,
        }
    }
}

impl MqttConfig {
    /// Read config from env vars. Unset vars fall back to defaults.
    ///
    /// Variables: MQTT_HOST, MQTT_PORT, MQTT_USERNAME, MQTT_PASSWORD
    pub fn from_env() -> Self {
        let host = std::env::var("MQTT_HOST").unwrap_or_else(|_| "localhost".to_string());
        let port = std::env::var("MQTT_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1883);
        let username = std::env::var("MQTT_USERNAME").ok().filter(|s| !s.is_empty());
        let password = std::env::var("MQTT_PASSWORD").ok().filter(|s| !s.is_empty());
        Self {
            host,
            port,
            username,
            password,
            ..Self::default()
        }
    }

    /// True when MQTT_HOST is set — used by main.rs to decide whether to wire the adapter.
    pub fn is_configured() -> bool {
        std::env::var("MQTT_HOST").is_ok()
    }
}
