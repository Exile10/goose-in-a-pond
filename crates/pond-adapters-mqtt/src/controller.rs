use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::domain::device::{DeviceCapability, DeviceState, StateValue};
use pond_core::ports::device_controller::DeviceController;
use pond_core::ports::device_registry::DeviceRegistry;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::config::MqttConfig;
use crate::topic::{tasmota_cache_key, TopicScheme};

/// MQTT implementation of `DeviceController`.
///
/// Maintains a live connection to a broker and a state cache populated from
/// wildcard subscriptions. Supports zigbee2mqtt, Tasmota, and generic JSON bridges.
pub struct MqttDeviceController {
    client: AsyncClient,
    /// State cache keyed by normalized device address (see `TopicScheme::cache_key`).
    state_cache: Arc<RwLock<HashMap<String, DeviceState>>>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
}

impl MqttDeviceController {
    /// Connect to the broker and start the background event-loop task.
    pub async fn new(
        config: MqttConfig,
        registry: Arc<dyn DeviceRegistry + Send + Sync>,
    ) -> Result<Self> {
        let mut opts = MqttOptions::new(&config.client_id, &config.host, config.port);
        opts.set_keep_alive(Duration::from_secs(config.keep_alive_secs));
        if let (Some(u), Some(p)) = (&config.username, &config.password) {
            opts.set_credentials(u, p);
        }

        let (client, eventloop) = AsyncClient::new(opts, 16);
        let state_cache: Arc<RwLock<HashMap<String, DeviceState>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Wildcard subscriptions catch state for all supported protocols without
        // needing to know registered devices up front.
        client.subscribe("zigbee2mqtt/+", QoS::AtMostOnce).await?;
        client.subscribe("stat/+/STATE", QoS::AtMostOnce).await?;
        client.subscribe("stat/+/RESULT", QoS::AtMostOnce).await?;

        let cache_clone = Arc::clone(&state_cache);
        tokio::spawn(run_event_loop(eventloop, cache_clone));

        Ok(Self { client, state_cache, registry })
    }

    async fn resolve_address(&self, device_id: &str) -> Result<String> {
        let device = self
            .registry
            .get_device(device_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("device '{device_id}' not found in registry"))?;
        let raw = device
            .address
            .ok_or_else(|| anyhow::anyhow!("device '{device_id}' has no MQTT address"))?;
        Ok(TopicScheme::cache_key(&raw))
    }
}

async fn run_event_loop(mut eventloop: EventLoop, cache: Arc<RwLock<HashMap<String, DeviceState>>>) {
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::Publish(p))) => {
                debug!(topic = %p.topic, bytes = p.payload.len(), "MQTT publish received");
                if let Some((key, state)) = parse_state_packet(&p.topic, &p.payload) {
                    cache.write().await.insert(key, state);
                }
            }
            Ok(_) => {}
            Err(e) => {
                warn!("MQTT connection error: {e:#}; retrying in 5 s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// Parse an incoming MQTT publish into a (cache_key, DeviceState) pair.
///
/// Returns None for topics/payloads we don't recognise (e.g. zigbee2mqtt bridge info).
fn parse_state_packet(topic: &str, payload: &[u8]) -> Option<(String, DeviceState)> {
    let text = std::str::from_utf8(payload).ok()?;
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = json.as_object()?;

    let cache_key = if topic.starts_with("zigbee2mqtt/") && !topic.ends_with("/set") {
        // Exclude the bridge meta topics (zigbee2mqtt/bridge/...)
        if topic.contains("/bridge/") {
            return None;
        }
        topic.to_string()
    } else if let Some(key) = tasmota_cache_key(topic) {
        key
    } else {
        return None;
    };

    let mut values = HashMap::new();
    for (k, v) in obj {
        let sv = match v {
            serde_json::Value::Bool(b) => StateValue::Bool(*b),
            serde_json::Value::Number(n) => StateValue::Number(n.as_f64()?),
            serde_json::Value::String(s) => match s.as_str() {
                "ON" | "on" => StateValue::Bool(true),
                "OFF" | "off" => StateValue::Bool(false),
                _ => StateValue::Text(s.clone()),
            },
            _ => continue,
        };
        values.insert(k.clone(), sv);
    }

    Some((
        cache_key.clone(),
        DeviceState { device_id: cache_key, values },
    ))
}

fn state_value_to_json(v: &StateValue) -> serde_json::Value {
    match v {
        StateValue::Bool(b) => serde_json::Value::Bool(*b),
        StateValue::Number(n) => serde_json::json!(*n),
        StateValue::Text(s) => serde_json::Value::String(s.clone()),
    }
}

#[async_trait]
impl DeviceController for MqttDeviceController {
    async fn set_power(&self, device_id: &str, on: bool) -> Result<()> {
        let addr = self.resolve_address(device_id).await?;
        let (topic, payload) = TopicScheme::from_address(&addr).power_set(on);
        self.client
            .publish(topic, QoS::AtLeastOnce, false, payload)
            .await
            .map_err(|e| anyhow::anyhow!("MQTT publish error: {e:#}"))
    }

    async fn set_state(&self, device_id: &str, key: &str, value: StateValue) -> Result<()> {
        let addr = self.resolve_address(device_id).await?;
        let json_val = state_value_to_json(&value);
        let (topic, payload) = TopicScheme::from_address(&addr).state_set(key, &json_val);
        self.client
            .publish(topic, QoS::AtLeastOnce, false, payload)
            .await
            .map_err(|e| anyhow::anyhow!("MQTT publish error: {e:#}"))
    }

    async fn query_state(&self, device_id: &str) -> Result<DeviceState> {
        let addr = self.resolve_address(device_id).await?;
        let cache = self.state_cache.read().await;
        Ok(cache.get(&addr).cloned().unwrap_or_else(|| DeviceState {
            device_id: addr,
            values: HashMap::new(),
        }))
    }

    async fn capabilities(&self, device_id: &str) -> Result<Vec<DeviceCapability>> {
        let device = self
            .registry
            .get_device(device_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("device '{device_id}' not found in registry"))?;
        Ok(device.structured_capabilities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_zigbee2mqtt_state_packet() {
        let payload =
            br#"{"state":"ON","brightness":200,"color_temp":370,"linkquality":82}"#;
        let (key, state) = parse_state_packet("zigbee2mqtt/lamp1", payload).unwrap();
        assert_eq!(key, "zigbee2mqtt/lamp1");
        assert_eq!(state.values["state"], StateValue::Bool(true));
        assert_eq!(state.values["brightness"], StateValue::Number(200.0));
    }

    #[test]
    fn parse_tasmota_state_packet() {
        let payload = br#"{"POWER":"ON","Wifi":{"RSSI":72}}"#;
        let (key, state) = parse_state_packet("stat/plug1/STATE", payload).unwrap();
        assert_eq!(key, "tasmota/plug1");
        assert_eq!(state.values["POWER"], StateValue::Bool(true));
    }

    #[test]
    fn bridge_meta_topic_is_ignored() {
        let payload = br#"{"type":"devices"}"#;
        assert!(parse_state_packet("zigbee2mqtt/bridge/devices", payload).is_none());
    }

    #[test]
    fn unknown_topic_returns_none() {
        assert!(parse_state_packet("homeassistant/sensor/foo", b"{}").is_none());
    }

    #[test]
    fn state_value_to_json_roundtrips() {
        assert_eq!(state_value_to_json(&StateValue::Bool(true)), serde_json::json!(true));
        assert_eq!(state_value_to_json(&StateValue::Number(42.5)), serde_json::json!(42.5));
        assert_eq!(
            state_value_to_json(&StateValue::Text("heat".to_string())),
            serde_json::json!("heat")
        );
    }
}
