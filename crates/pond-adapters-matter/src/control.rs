//! [`MatterDeviceControl`] — the [`DeviceControlPort`] over a live controller
//! connection.
//!
//! Every verb is one `control` op. Which cluster that becomes, which endpoint it
//! lands on, and what unit the value is in are all the controller's business —
//! it has matter.js's typed cluster models to decide with, where this crate had
//! a hand-maintained table of decimal cluster ids.
//!
//! The outcome is built from what the controller says it applied, not from what
//! the caller asked for. That distinction is the reason the wire carries an
//! `applied` patch at all: reporting the request back as though it were the
//! result is how a device that rejected a write still got described to the user
//! as having taken it.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_core::user_data::ports::device_control::{
    DeviceControlOutcome, DeviceControlPort, DeviceDescription, DeviceState,
};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::client::{code_of, MatterClient};
use crate::protocol::{describe, ControlResult, DescribeResult, StateResult};

/// A swappable handle to the live client. The reconnect supervisor replaces the
/// inner `Arc<MatterClient>` after re-establishing the WebSocket, so the control
/// port keeps working across a controller restart without being rebuilt.
/// Reads clone the current `Arc` and drop the lock immediately.
pub type SharedMatterClient = Arc<RwLock<Arc<MatterClient>>>;

pub struct MatterDeviceControl {
    client: SharedMatterClient,
}

impl MatterDeviceControl {
    pub fn new(client: Arc<MatterClient>) -> Self {
        Self {
            client: Arc::new(RwLock::new(client)),
        }
    }

    /// The swappable client handle, so the reconnect supervisor can replace the
    /// underlying connection in place after a drop.
    pub fn client_handle(&self) -> SharedMatterClient {
        self.client.clone()
    }

    /// Drive one verb and report what the device became.
    ///
    /// This is the only place a device command is logged, and it is logged
    /// whichever way it goes: a control path that says nothing on success gives
    /// no way to tell "the command was never sent" from "the device ignored it",
    /// which was the whole diagnostic position before.
    async fn control(
        &self,
        device_id: &str,
        verb: &str,
        value: Value,
    ) -> Result<DeviceControlOutcome> {
        let client = self.client.read().await.clone();
        let started = Instant::now();

        let outcome = client
            .send(
                "control",
                json!({ "device_id": device_id, "verb": verb, "value": value }),
            )
            .await;

        let elapsed = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(result) => {
                let applied = serde_json::from_value::<ControlResult>(result)
                    .map(|r| r.applied)
                    .unwrap_or_default();
                tracing::debug!(
                    target: "giap::trace",
                    kind = "matter_device_command",
                    device = %device_id,
                    verb,
                    duration_ms = elapsed,
                    "drove a Matter device"
                );
                Ok(DeviceControlOutcome::new(device_id, applied))
            }
            Err(e) => {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "matter_device_command",
                    device = %device_id,
                    verb,
                    duration_ms = elapsed,
                    error_code = code_of(&e).unwrap_or("none"),
                    error = %describe(&e),
                    "a Matter device command failed"
                );
                Err(e)
            }
        }
    }
}

#[async_trait]
impl DeviceControlPort for MatterDeviceControl {
    async fn describe(&self, device_id: &str) -> Result<DeviceDescription> {
        let client = self.client.read().await.clone();
        let result = client
            .send("describe", json!({ "device_id": device_id }))
            .await?;
        serde_json::from_value::<DescribeResult>(result)
            .map(|r| r.description)
            .context("the controller did not describe the device")
    }

    async fn state(&self, device_id: &str) -> Result<DeviceState> {
        let client = self.client.read().await.clone();
        let result = client
            .send("state", json!({ "device_id": device_id }))
            .await?;
        serde_json::from_value::<StateResult>(result)
            .map(|r| r.state)
            .context("the controller did not report the device's state")
    }

    async fn set_power(&self, device_id: &str, on: bool) -> Result<DeviceControlOutcome> {
        self.control(device_id, "power", json!(on)).await
    }

    async fn set_brightness(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        self.control(device_id, "brightness", json!(percent.min(100)))
            .await
    }

    async fn set_target_temp(&self, device_id: &str, celsius: f32) -> Result<DeviceControlOutcome> {
        self.control(device_id, "target_temp", json!(celsius)).await
    }

    async fn set_locked(&self, device_id: &str, locked: bool) -> Result<DeviceControlOutcome> {
        self.control(device_id, "locked", json!(locked)).await
    }

    async fn set_color(
        &self,
        device_id: &str,
        hue_degrees: u16,
        saturation_percent: u8,
    ) -> Result<DeviceControlOutcome> {
        self.control(
            device_id,
            "color",
            json!({ "hue": hue_degrees, "saturation": saturation_percent.min(100) }),
        )
        .await
    }

    async fn set_volume(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        self.control(device_id, "volume", json!(percent.min(100)))
            .await
    }

    async fn set_color_temp(&self, device_id: &str, kelvin: u32) -> Result<DeviceControlOutcome> {
        self.control(device_id, "color_temp", json!(kelvin)).await
    }

    async fn set_fan_speed(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        self.control(device_id, "fan_speed", json!(percent.min(100)))
            .await
    }

    async fn set_fan_mode(&self, device_id: &str, mode: &str) -> Result<DeviceControlOutcome> {
        self.control(device_id, "fan_mode", json!(mode)).await
    }

    async fn set_mode(
        &self,
        device_id: &str,
        setting: &str,
        value: &str,
    ) -> Result<DeviceControlOutcome> {
        self.control(
            device_id,
            "mode",
            json!({ "setting": setting, "value": value }),
        )
        .await
    }

    async fn set_operation(
        &self,
        device_id: &str,
        operation: &str,
    ) -> Result<DeviceControlOutcome> {
        self.control(device_id, "operation", json!(operation)).await
    }

    async fn set_tilt(&self, device_id: &str, percent_open: u8) -> Result<DeviceControlOutcome> {
        self.control(device_id, "tilt", json!(percent_open.min(100)))
            .await
    }

    async fn set_position(
        &self,
        device_id: &str,
        percent_open: u8,
    ) -> Result<DeviceControlOutcome> {
        self.control(device_id, "position", json!(percent_open.min(100)))
            .await
    }
}
