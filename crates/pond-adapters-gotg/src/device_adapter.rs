//! GOTG Device Adapter
//!
//! Implements the DeviceRegistry port for managing GOTG mobile devices.
//!
//! # TODO
//! - [ ] Store devices in the system DB
//! - [ ] Add device capabilities matching
//! - [ ] Add device presence tracking (heartbeat intervals)
//! - [ ] Add device removal cleanup

use anyhow::Result;
use async_trait::async_trait;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};

/// GOTG-specific device registry adapter.
pub struct GotgDeviceAdapter {
    // TODO: Add DB pool reference
}

impl GotgDeviceAdapter {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl DeviceRegistry for GotgDeviceAdapter {
    async fn register(&self, request: RegisterDeviceRequest) -> Result<Device> {
        // TODO: Insert into system DB
        let device = Device {
            id: uuid::Uuid::new_v4().to_string(),
            name: request.name,
            device_type: request.device_type,
            hostname: request.hostname,
            ip_address: None,
            capabilities: request.capabilities,
            registered_at: chrono::Utc::now().to_rfc3339(),
            last_seen: Some(chrono::Utc::now().to_rfc3339()),
            is_online: true,
            room: request.room,
        };

        tracing::info!("Registered device: {} ({})", device.name, device.id);
        Ok(device)
    }

    async fn list_devices(&self) -> Result<Vec<Device>> {
        // TODO: Query system DB
        Ok(vec![])
    }

    async fn get_device(&self, _device_id: &str) -> Result<Option<Device>> {
        // TODO: Query system DB
        Ok(None)
    }

    async fn unregister(&self, device_id: &str) -> Result<()> {
        // TODO: Delete from system DB
        tracing::info!("Unregistered device: {}", device_id);
        Ok(())
    }

    async fn heartbeat(&self, device_id: &str) -> Result<()> {
        // TODO: Update last_seen in system DB
        tracing::debug!("Heartbeat from device: {}", device_id);
        Ok(())
    }
}
