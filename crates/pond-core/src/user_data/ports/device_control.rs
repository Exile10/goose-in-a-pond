//! Driven Port: Device Control
//!
//! The actuation seam for smart devices. The agent (and the desktop Hub via
//! `POST /api/v1/tools/invoke`) reaches devices only through the
//! `giap-device-control` MCP tool, which calls this port. Concrete backends
//! (MQTT / HTTP / IR, or a Home-Assistant MCP-client) implement it; a logging
//! stub is the default until a real adapter is wired.
//!
//! Capability-typed (no opaque JSON state) so the control boundary is
//! machine-checkable end-to-end.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The device state produced by a control action — echoed back so callers
/// (e.g. the Hub overlay) can reconcile their optimistic UI with reality.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceStatePatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    /// Brightness as a 0–100 percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    /// Target temperature in degrees Celsius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_temp: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    /// Colour hue in degrees (0–360).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hue: Option<u16>,
    /// Colour saturation as a 0–100 percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saturation: Option<u8>,
    /// Fan speed as a 0–100 percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_speed: Option<u8>,
    /// Fan mode by name: off, low, medium, high, on, auto, smart. A fan's speed
    /// and its mode are the same control seen two ways — a device asked for
    /// "auto" has no percentage to report, which is why this is not a number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_mode: Option<String>,
    /// Covering position as a 0–100 percentage **open** (100 = fully open).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<u8>,
    /// The named setting that changed, and what it became.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeChange>,
    /// The operation that was run: start, stop, pause or resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

/// A named setting and its new value, both in the device's own words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeChange {
    pub setting: String,
    pub value: String,
}

/// Outcome of a control action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceControlOutcome {
    pub device_id: String,
    /// The resulting state after the action (best-effort echo).
    pub applied: DeviceStatePatch,
}

impl DeviceControlOutcome {
    pub fn new(device_id: impl Into<String>, applied: DeviceStatePatch) -> Self {
        Self {
            device_id: device_id.into(),
            applied,
        }
    }
}

/// One thing a device currently is: `spin speed` is `High`.
///
/// `name` is always a name [`DeviceDescription`] also uses — a control verb for a
/// scalar, a setting name for a selectable — so a reading names the thing that
/// changes it, and reading leads to acting without a second lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateValue {
    pub name: String,
    pub value: String,
}

/// Everything a device currently reports.
///
/// The counterpart to [`DeviceDescription`]: that says what a device can be told to
/// do, this says what it is doing. Without it the only way to learn a device's state
/// was to change it, and "is the washer running?" had no answer that did not involve
/// starting the washer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceState {
    pub device_id: String,
    pub values: Vec<StateValue>,
}

/// What a device can be told to do and what it measures, in its own terms.
///
/// `Device::capabilities` is a list of verb names — enough to know a fan has a
/// speed, not enough to drive it. It cannot say which modes that fan has, what a
/// thermostat's limits are, or that an air quality sensor measures eleven
/// substances. An agent given only the list guesses, and learns the limits by
/// failing at them in front of the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceDescription {
    pub device_id: String,
    pub device_type: String,
    /// Verbs the device accepts, named as this port names them.
    pub capabilities: Vec<Capability>,
    /// What it measures, whether or not it has reported yet.
    pub sensors: Vec<SensorSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    pub verb: String,
    /// Which named setting this is, for a verb a device offers more than once.
    ///
    /// A washer has four `mode` capabilities — wash cycle, temperature level, spin
    /// speed, rinses — and this is the name that tells them apart, and the same name
    /// [`DeviceControlPort::set_mode`] is called with. Absent for a verb a device can
    /// only have one of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
    pub value: ValueSpec,
}

/// The shape a verb accepts. A constraint is present only when the device stated
/// it: an invented range is worse than an absent one, because it is believed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValueSpec {
    Boolean,
    /// 0–100.
    Percent,
    Number {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    Enum {
        values: Vec<String>,
    },
    Color,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensorSpec {
    pub sensor_type: String,
    pub unit: String,
}

/// Driven Port: actuate a smart device.
#[async_trait]
pub trait DeviceControlPort: Send + Sync {
    /// Turn a device on or off.
    async fn set_power(&self, device_id: &str, on: bool) -> Result<DeviceControlOutcome>;

    /// Set brightness as a 0–100 percentage.
    async fn set_brightness(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome>;

    /// Set a thermostat target temperature in degrees Celsius.
    async fn set_target_temp(&self, device_id: &str, celsius: f32) -> Result<DeviceControlOutcome>;

    /// Lock or unlock a device.
    async fn set_locked(&self, device_id: &str, locked: bool) -> Result<DeviceControlOutcome>;

    // ── Optional capabilities ────────────────────────────────────────────
    //
    // Not every backend speaks these. They default to an "unsupported" error so
    // a backend opts in by overriding, rather than every implementor being
    // forced to write a stub. The MCP tool surfaces the error to the user as a
    // plain "this device can't do that".

    /// Set colour by hue (0–360 degrees) and saturation (0–100 percent).
    async fn set_color(
        &self,
        device_id: &str,
        _hue_degrees: u16,
        _saturation_percent: u8,
    ) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support colour control")
    }

    /// Set fan speed as a 0–100 percentage.
    async fn set_fan_speed(&self, device_id: &str, _percent: u8) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support fan control")
    }

    /// Set fan mode by name: off, low, medium, high, on, auto, smart.
    ///
    /// Separate from [`Self::set_fan_speed`] because auto and smart are not
    /// points on the percentage scale — they hand the choice back to the
    /// device, which is exactly what a user asking for "auto" wants.
    async fn set_fan_mode(&self, device_id: &str, _mode: &str) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support fan modes")
    }

    /// What this device can be told to do, and what it measures.
    ///
    /// Optional like the verbs below: a transport that cannot ask a device about
    /// itself says so rather than inventing an answer. The Matter adapter reads
    /// it live from the controller, so it reflects the device as it is now
    /// rather than as it was when it was paired.
    async fn describe(&self, device_id: &str) -> Result<DeviceDescription> {
        anyhow::bail!("device '{device_id}' does not describe itself")
    }

    /// What this device currently is.
    ///
    /// Optional in the same way the verbs below are: a backend that cannot read a
    /// device's state says so rather than returning an empty one, which would be
    /// indistinguishable from a device reporting nothing.
    async fn state(&self, device_id: &str) -> Result<DeviceState> {
        anyhow::bail!("device '{device_id}' cannot report its state")
    }

    /// Choose a named setting — a wash cycle, a spin speed, a temperature level.
    ///
    /// One verb rather than one per appliance: Matter's appliance controls are
    /// nearly all the same shape, a list of choices the device publishes. The
    /// setting name and the value both come from [`Self::describe`], so what is
    /// describable is callable.
    async fn set_mode(
        &self,
        device_id: &str,
        _setting: &str,
        _value: &str,
    ) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' has no settings that can be chosen")
    }

    /// Start, stop, pause or resume a device that runs cycles.
    async fn set_operation(
        &self,
        device_id: &str,
        _operation: &str,
    ) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not run cycles")
    }

    /// Set a covering (blind/curtain/shade) position, as a 0–100 percentage
    /// **open** — 100 is fully open, 0 fully closed.
    async fn set_position(
        &self,
        device_id: &str,
        _percent_open: u8,
    ) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support position control")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::mocks::mock_device_control::RecordingDeviceControl;

    #[tokio::test]
    async fn set_power_records_and_echoes() {
        let dc = RecordingDeviceControl::default();
        let out = dc.set_power("lamp-1", true).await.unwrap();
        assert_eq!(out.device_id, "lamp-1");
        assert_eq!(out.applied.on, Some(true));
        assert_eq!(
            dc.last_call().as_deref(),
            Some("set_power(lamp-1, on=true)")
        );
    }

    #[tokio::test]
    async fn set_brightness_implies_on_when_positive() {
        let dc = RecordingDeviceControl::default();
        let out = dc.set_brightness("lamp-1", 40).await.unwrap();
        assert_eq!(out.applied.brightness, Some(40));
        assert_eq!(out.applied.on, Some(true));

        let off = dc.set_brightness("lamp-1", 0).await.unwrap();
        assert_eq!(off.applied.on, Some(false));
    }

    #[tokio::test]
    async fn set_target_temp_and_locked_echo() {
        let dc = RecordingDeviceControl::default();
        let t = dc.set_target_temp("thermo", 21.5).await.unwrap();
        assert_eq!(t.applied.target_temp, Some(21.5));

        let l = dc.set_locked("door", true).await.unwrap();
        assert_eq!(l.applied.locked, Some(true));
    }
}
