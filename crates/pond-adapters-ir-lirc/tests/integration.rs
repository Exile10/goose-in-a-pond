//! Integration tests — require a real LIRC installation and lircd running.
//! Run with: cargo test -p pond-adapters-ir-lirc -- --ignored

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use pond_adapters_ir_lirc::{IrConfig, IrDeviceController};
use pond_core::domain::device::StateValue;
use pond_core::ports::device_controller::DeviceController;
use pond_core::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};

struct StubRegistry {
    device: Device,
}

#[async_trait]
impl DeviceRegistry for StubRegistry {
    async fn register(&self, _: RegisterDeviceRequest) -> Result<Device> {
        Err(anyhow::anyhow!("stub"))
    }
    async fn list_devices(&self) -> Result<Vec<Device>> {
        Ok(vec![self.device.clone()])
    }
    async fn get_device(&self, id: &str) -> Result<Option<Device>> {
        if self.device.id == id {
            Ok(Some(self.device.clone()))
        } else {
            Ok(None)
        }
    }
    async fn unregister(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

fn test_device() -> Device {
    Device {
        id: "tv-test".into(),
        name: "Test TV".into(),
        device_type: "tv".into(),
        hostname: None,
        ip_address: None,
        capabilities: vec!["ir".into()],
        transport: "lirc".into(),
        // Set IR_TEST_REMOTE env var to your lircd remote name (e.g. SAMSUNG_TV)
        address: Some(
            std::env::var("IR_TEST_REMOTE").unwrap_or_else(|_| "TEST_REMOTE".into()),
        ),
        structured_capabilities: pond_adapters_ir_lirc::tv_capabilities(),
        registered_at: "2025-01-01T00:00:00Z".into(),
        last_seen: None,
        is_online: true,
    }
}

fn make_controller() -> IrDeviceController {
    let registry = Arc::new(StubRegistry { device: test_device() });
    IrDeviceController::new(IrConfig::from_env(), registry)
}

/// Sends KEY_POWER to the test remote.
/// Set IR_TEST_REMOTE to your lircd remote name.
#[tokio::test]
#[ignore]
async fn test_power_toggle() {
    let ctrl = make_controller();
    ctrl.set_power("tv-test", true)
        .await
        .expect("IR power toggle should succeed with LIRC running");
}

/// Sends KEY_VOLUMEUP three times.
#[tokio::test]
#[ignore]
async fn test_volume_up_three_steps() {
    let ctrl = make_controller();
    ctrl.set_state("tv-test", "volume_up", StateValue::Number(3.0))
        .await
        .expect("IR volume up should succeed");
}

/// Sends KEY_MUTE.
#[tokio::test]
#[ignore]
async fn test_mute_toggle() {
    let ctrl = make_controller();
    ctrl.set_state("tv-test", "mute", StateValue::Bool(true))
        .await
        .expect("IR mute should succeed");
}

/// query_state should always return empty (no feedback channel).
#[tokio::test]
#[ignore]
async fn test_query_state_is_empty() {
    let ctrl = make_controller();
    let state = ctrl.query_state("tv-test").await.unwrap();
    assert!(state.values.is_empty(), "IR state should always be empty");
}
