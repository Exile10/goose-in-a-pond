use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use crate::domain::device::{DeviceCapability, DeviceState, StateValue};
use crate::ports::device_controller::DeviceController;

/// Test double — stores state in memory for assertion in unit tests.
pub struct MockDeviceController {
    states: Mutex<HashMap<String, DeviceState>>,
    capabilities: Mutex<HashMap<String, Vec<DeviceCapability>>>,
}

impl MockDeviceController {
    pub fn new() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            capabilities: Mutex::new(HashMap::new()),
        }
    }

    /// Pre-load capabilities for a device so `capabilities()` returns them.
    pub fn seed_capabilities(&self, device_id: &str, caps: Vec<DeviceCapability>) {
        self.capabilities
            .lock()
            .unwrap()
            .insert(device_id.to_string(), caps);
    }

    /// Inspect stored state for a device (used in tests).
    pub fn get_state(&self, device_id: &str) -> Option<DeviceState> {
        self.states.lock().unwrap().get(device_id).cloned()
    }
}

impl Default for MockDeviceController {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DeviceController for MockDeviceController {
    async fn set_power(&self, device_id: &str, on: bool) -> Result<()> {
        let mut states = self.states.lock().unwrap();
        let state = states
            .entry(device_id.to_string())
            .or_insert_with(|| DeviceState {
                device_id: device_id.to_string(),
                values: HashMap::new(),
            });
        state.values.insert("power".to_string(), StateValue::Bool(on));
        Ok(())
    }

    async fn set_state(&self, device_id: &str, key: &str, value: StateValue) -> Result<()> {
        let mut states = self.states.lock().unwrap();
        let state = states
            .entry(device_id.to_string())
            .or_insert_with(|| DeviceState {
                device_id: device_id.to_string(),
                values: HashMap::new(),
            });
        state.values.insert(key.to_string(), value);
        Ok(())
    }

    async fn query_state(&self, device_id: &str) -> Result<DeviceState> {
        let states = self.states.lock().unwrap();
        Ok(states.get(device_id).cloned().unwrap_or_else(|| DeviceState {
            device_id: device_id.to_string(),
            values: HashMap::new(),
        }))
    }

    async fn capabilities(&self, device_id: &str) -> Result<Vec<DeviceCapability>> {
        let caps = self.capabilities.lock().unwrap();
        Ok(caps.get(device_id).cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::device::CapabilityKind;

    #[tokio::test]
    async fn set_power_on_stores_bool_true() {
        let ctrl = MockDeviceController::new();
        ctrl.set_power("lamp-1", true).await.unwrap();
        let state = ctrl.query_state("lamp-1").await.unwrap();
        assert_eq!(state.values["power"], StateValue::Bool(true));
    }

    #[tokio::test]
    async fn set_power_off_overwrites_previous() {
        let ctrl = MockDeviceController::new();
        ctrl.set_power("lamp-1", true).await.unwrap();
        ctrl.set_power("lamp-1", false).await.unwrap();
        let state = ctrl.query_state("lamp-1").await.unwrap();
        assert_eq!(state.values["power"], StateValue::Bool(false));
    }

    #[tokio::test]
    async fn set_state_stores_number_value() {
        let ctrl = MockDeviceController::new();
        ctrl.set_state("lamp-1", "brightness", StateValue::Number(75.0))
            .await
            .unwrap();
        let state = ctrl.query_state("lamp-1").await.unwrap();
        assert_eq!(state.values["brightness"], StateValue::Number(75.0));
    }

    #[tokio::test]
    async fn set_state_stores_text_value() {
        let ctrl = MockDeviceController::new();
        ctrl.set_state("thermostat-1", "mode", StateValue::Text("heat".to_string()))
            .await
            .unwrap();
        let state = ctrl.query_state("thermostat-1").await.unwrap();
        assert_eq!(
            state.values["mode"],
            StateValue::Text("heat".to_string())
        );
    }

    #[tokio::test]
    async fn query_state_returns_empty_for_unknown_device() {
        let ctrl = MockDeviceController::new();
        let state = ctrl.query_state("nonexistent").await.unwrap();
        assert!(state.values.is_empty());
        assert_eq!(state.device_id, "nonexistent");
    }

    #[tokio::test]
    async fn capabilities_returns_seeded_values() {
        let ctrl = MockDeviceController::new();
        ctrl.seed_capabilities(
            "thermostat-1",
            vec![
                DeviceCapability {
                    key: "power".to_string(),
                    kind: CapabilityKind::Toggle,
                },
                DeviceCapability {
                    key: "temperature".to_string(),
                    kind: CapabilityKind::Range {
                        min: 15.0,
                        max: 30.0,
                        step: Some(0.5),
                    },
                },
                DeviceCapability {
                    key: "mode".to_string(),
                    kind: CapabilityKind::Enum {
                        options: vec!["heat".to_string(), "cool".to_string(), "fan".to_string()],
                    },
                },
            ],
        );

        let caps = ctrl.capabilities("thermostat-1").await.unwrap();
        assert_eq!(caps.len(), 3);
        assert_eq!(caps[0].key, "power");
        assert!(matches!(caps[0].kind, CapabilityKind::Toggle));
    }

    #[tokio::test]
    async fn capabilities_returns_empty_for_unseeded_device() {
        let ctrl = MockDeviceController::new();
        let caps = ctrl.capabilities("unknown").await.unwrap();
        assert!(caps.is_empty());
    }

    #[tokio::test]
    async fn independent_devices_do_not_share_state() {
        let ctrl = MockDeviceController::new();
        ctrl.set_power("device-a", true).await.unwrap();
        let state_b = ctrl.query_state("device-b").await.unwrap();
        assert!(state_b.values.is_empty());
    }

    #[tokio::test]
    async fn trait_object_conformance() {
        use std::sync::Arc;
        let _: Arc<dyn DeviceController> = Arc::new(MockDeviceController::new());
    }
}
