use anyhow::Result;
use async_trait::async_trait;
use crate::domain::device::{DeviceCapability, DeviceState, StateValue};

/// Driven Port: protocol-agnostic device control.
///
/// Adapters implement this for each home-automation protocol
/// (e.g. Home Assistant REST, Matter, KNX). The Core never
/// speaks a protocol directly — all control crosses this port.
#[async_trait]
pub trait DeviceController: Send + Sync {
    /// Turn the device on (`true`) or off (`false`).
    async fn set_power(&self, device_id: &str, on: bool) -> Result<()>;

    /// Write an arbitrary state key. The key space is defined by `capabilities()`.
    async fn set_state(&self, device_id: &str, key: &str, value: StateValue) -> Result<()>;

    /// Return the full current state snapshot for the device.
    async fn query_state(&self, device_id: &str) -> Result<DeviceState>;

    /// Return the capabilities the device exposes through this controller.
    async fn capabilities(&self, device_id: &str) -> Result<Vec<DeviceCapability>>;
}
