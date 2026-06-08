//! Driven Port: Device Registry
//!
//! Manages connected devices — mobile phones (GOTG), smart home devices,
//! other pond instances, etc.
//!
//! # TODO
//! - [ ] Add device grouping / rooms

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::domain::device::DeviceCapability;

fn default_transport() -> String {
    "http".to_string()
}

/// A registered device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// "gotg", "smart_speaker", "sensor", "pond", etc.
    pub device_type: String,
    pub hostname: Option<String>,
    pub ip_address: Option<String>,
    /// Simple capability tags (e.g. "chat", "tts"). Kept for labelling / filtering.
    pub capabilities: Vec<String>,
    /// Wire protocol used to reach the device: "http", "mqtt", "ws", "gotg", "matter", …
    #[serde(default = "default_transport")]
    pub transport: String,
    /// Protocol-specific endpoint: URL, MQTT broker+topic, hostname:port, etc.
    #[serde(default)]
    pub address: Option<String>,
    /// Structured control descriptors for use by a DeviceController adapter.
    #[serde(default)]
    pub structured_capabilities: Vec<DeviceCapability>,
    pub registered_at: String,
    pub last_seen: Option<String>,
    pub is_online: bool,
}

/// Request to register a new device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterDeviceRequest {
    pub name: String,
    pub device_type: String,
    pub hostname: Option<String>,
    pub capabilities: Vec<String>,
    /// Wire protocol used to reach the device (defaults to "http").
    #[serde(default = "default_transport")]
    pub transport: String,
    /// Protocol-specific endpoint address.
    #[serde(default)]
    pub address: Option<String>,
    /// Structured control descriptors.
    #[serde(default)]
    pub structured_capabilities: Vec<DeviceCapability>,
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
