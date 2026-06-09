//! Integration tests against a live MQTT broker.
//!
//! These tests are `#[ignore]` by default — run them against a local broker:
//!
//!   docker run -d -p 1883:1883 eclipse-mosquitto
//!   cargo test -p pond-adapters-mqtt -- --ignored

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use pond_adapters_mqtt::{MqttConfig, MqttDeviceController};
use pond_core::domain::device::{DeviceCapability, DeviceState, StateValue, CapabilityKind};
use pond_core::ports::device_controller::DeviceController;
use pond_core::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};

/// In-memory registry stub for tests.
struct StubRegistry {
    device: Device,
}

impl StubRegistry {
    fn with_address(id: &str, name: &str, address: &str) -> Self {
        Self {
            device: Device {
                id: id.to_string(),
                name: name.to_string(),
                device_type: "mqtt-device".to_string(),
                hostname: None,
                ip_address: None,
                capabilities: vec![],
                transport: "mqtt".to_string(),
                address: Some(address.to_string()),
                structured_capabilities: vec![
                    DeviceCapability {
                        key: "power".to_string(),
                        kind: CapabilityKind::Toggle,
                    },
                    DeviceCapability {
                        key: "brightness".to_string(),
                        kind: CapabilityKind::Range { min: 0.0, max: 254.0, step: Some(1.0) },
                    },
                ],
                registered_at: "2026-01-01T00:00:00Z".to_string(),
                last_seen: None,
                is_online: true,
            },
        }
    }
}

#[async_trait]
impl DeviceRegistry for StubRegistry {
    async fn register(&self, _: RegisterDeviceRequest) -> Result<Device> {
        Ok(self.device.clone())
    }
    async fn list_devices(&self) -> Result<Vec<Device>> {
        Ok(vec![self.device.clone()])
    }
    async fn get_device(&self, _id: &str) -> Result<Option<Device>> {
        Ok(Some(self.device.clone()))
    }
    async fn unregister(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

async fn make_controller(address: &str) -> MqttDeviceController {
    let config = MqttConfig::from_env(); // picks up MQTT_HOST / MQTT_PORT
    let registry = Arc::new(StubRegistry::with_address("dev-1", "test-device", address));
    MqttDeviceController::new(config, registry)
        .await
        .expect("failed to connect to MQTT broker — is it running?")
}

#[tokio::test]
#[ignore]
async fn zigbee2mqtt_set_power_publishes_to_broker() {
    let ctrl = make_controller("zigbee2mqtt/test_lamp").await;

    ctrl.set_power("dev-1", true)
        .await
        .expect("set_power should not fail");

    // Allow the broker to echo state back
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Smoke-test: query_state doesn't error even if broker hasn't echoed yet
    let _state = ctrl.query_state("dev-1").await.unwrap();
}

#[tokio::test]
#[ignore]
async fn zigbee2mqtt_set_brightness_encodes_json() {
    let ctrl = make_controller("zigbee2mqtt/test_lamp").await;

    ctrl.set_state("dev-1", "brightness", StateValue::Number(128.0))
        .await
        .expect("set_state brightness should not fail");
}

#[tokio::test]
#[ignore]
async fn tasmota_set_power_publishes_to_cmnd() {
    let ctrl = make_controller("tasmota/test_plug").await;

    ctrl.set_power("dev-1", false)
        .await
        .expect("set_power off should not fail");
}

#[tokio::test]
#[ignore]
async fn capabilities_returns_structured_caps_from_registry() {
    let ctrl = make_controller("zigbee2mqtt/test_lamp").await;
    let caps = ctrl.capabilities("dev-1").await.unwrap();
    assert_eq!(caps.len(), 2);
    assert_eq!(caps[0].key, "power");
    assert!(matches!(caps[0].kind, CapabilityKind::Toggle));
}
