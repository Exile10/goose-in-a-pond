//! Driven Port: Device Registry
//!
//! Manages connected devices — mobile phones (GOTG), smart home devices,
//! other pond instances, etc.
//!
//! # TODO
//! - [ ] Add device status (online/offline/last_seen)
//! - [ ] Add device capabilities discovery

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A registered device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// "gotg", "smart_speaker", "sensor", "pond", etc.
    pub device_type: String,
    pub hostname: Option<String>,
    pub ip_address: Option<String>,
    pub capabilities: Vec<String>,
    pub registered_at: String,
    pub last_seen: Option<String>,
    pub is_online: bool,
    /// Optional room grouping for hub UI ("Living Room", "Kitchen", …).
    /// `None` means the device shows under the default "Home" room.
    #[serde(default)]
    pub room: Option<String>,
}

/// Request to register a new device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterDeviceRequest {
    /// Stable caller-supplied id (e.g. `"matter-3"` from the Matter bridge,
    /// #195) so re-syncs address the same row. `None` = the registry
    /// generates a UUID, which remains the default for API registrations.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub device_type: String,
    pub hostname: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub room: Option<String>,
}

/// Driven Port: device lifecycle management.
#[async_trait]
pub trait DeviceRegistry: Send + Sync {
    /// Register a new device with the pond.
    async fn register(&self, request: RegisterDeviceRequest) -> Result<Device>;

    /// List all registered devices.
    async fn list_devices(&self) -> Result<Vec<Device>>;

    /// Get a specific device by ID.
    async fn get_device(&self, device_id: &str) -> Result<Option<Device>>;

    /// Remove a device registration.
    async fn unregister(&self, device_id: &str) -> Result<()>;

    /// Update device last-seen / online status.
    async fn heartbeat(&self, device_id: &str) -> Result<()>;
}
