//! No-op mock implementation of `DeviceRegistry` for use in tests and convenience factories.

use crate::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use anyhow::Result;
use async_trait::async_trait;

/// No-op device registry — always returns an empty list.
/// Use for tests that do not exercise home-control features.
pub struct MockDeviceRegistry;

#[async_trait]
impl DeviceRegistry for MockDeviceRegistry {
    async fn register(&self, _: RegisterDeviceRequest) -> Result<Device> {
        Err(anyhow::anyhow!("MockDeviceRegistry: register not implemented"))
    }

    async fn list_devices(&self) -> Result<Vec<Device>> {
        Ok(vec![])
    }

    async fn get_device(&self, _: &str) -> Result<Option<Device>> {
        Ok(None)
    }

    async fn unregister(&self, _: &str) -> Result<()> {
        Ok(())
    }

    async fn heartbeat(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
