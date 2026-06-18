//! Driven Port: device control (#84).
//!
//! The protocol-agnostic seam between the Core and any device-control adapter
//! (MQTT/zigbee2mqtt, HTTP/Shelly, IR blaster, …). Per the hexagonal rules,
//! control must cross this port before any protocol adapter exists — so the
//! mock + tests land first and the adapters (Q2-09…Q2-12) implement this trait.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::device::{DeviceCapability, DeviceId, DeviceState, DeviceStateValue};

/// Driven Port: control a device and read back its state, independent of the
/// underlying transport/protocol.
#[async_trait]
pub trait DeviceController: Send + Sync {
    /// Turn a device on or off (the canonical `power` capability).
    async fn set_power(&self, device: &DeviceId, on: bool) -> Result<()>;

    /// Set a single typed state value (e.g. `brightness` → `Int(80)`).
    async fn set_state(&self, device: &DeviceId, key: &str, value: DeviceStateValue) -> Result<()>;

    /// Read a device's current full state.
    async fn query_state(&self, device: &DeviceId) -> Result<DeviceState>;

    /// The capabilities a device advertises (used to validate/negotiate control).
    async fn capabilities(&self, device: &DeviceId) -> Result<Vec<DeviceCapability>>;
}
