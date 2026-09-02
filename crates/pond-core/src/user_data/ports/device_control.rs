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
    /// Speaker level as a 0–100 percentage. A different control from brightness, even
    /// though Matter carries both on the same cluster — the endpoint says which.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u8>,
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
    /// Colour temperature in KELVIN — warm white to cool white.
    ///
    /// Kelvin, not the cluster's mireds: kelvin is what a person says, and the
    /// controller owns the conversion for the same reason it owns every other unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_temp: Option<u32>,
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
    /// Slat angle as a 0–100 percentage **open**, a covering's second axis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<u8>,
    /// A valve, open (`true`) or shut (`false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valve: Option<bool>,
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
    /// Manufacturer-specific controls: present, and not drivable.
    ///
    /// Deliberately not [`Capability`] entries — a capability is a verb
    /// [`DeviceControlPort`] accepts, and there is none for these. They are carried
    /// because the alternative reads worse than silence: a description listing power
    /// and brightness for a device whose own app shows a third control states that
    /// the third does not exist, and gets believed. Empty for all but a few devices.
    #[serde(default)]
    pub vendor_clusters: Vec<VendorCluster>,
    /// What the device reports and nothing can set.
    ///
    /// The third kind of thing a device has. [`Capability`] is a verb this port
    /// accepts; [`SensorSpec`] is a numeric measurement. A door's position is neither
    /// — a word the lock reports, writable by nobody — so it fell through both, and a
    /// lock that can say "jammed" or "forced open" was described as one boolean.
    ///
    /// Read-only by construction, not by convention. Every writable attribute on
    /// Matter's DoorLock cluster is a security control, and a verb for one of them puts
    /// a lock's security configuration one sentence of natural language away.
    #[serde(default)]
    pub states: Vec<StateSpec>,
}

/// Something a device reports under a name, which nothing can write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateSpec {
    /// The name [`DeviceControlPort::state`] reports it under.
    pub name: String,
    /// The words it takes. An enum here is the closed list of what `state` may say,
    /// declared by the description so the two cannot drift.
    pub value: ValueSpec,
}

/// A control the device has and nothing here can name.
///
/// An id and an endpoint is the whole of it, and deliberately so. Matter publishes no
/// attribute names — "Flip-Flop" and "Emoticon" exist only in that maker's app — and a
/// controller discovers no shape for a cluster it cannot name either, so there is not
/// even a count of them to carry. Naming what is not there is how this class of bug
/// started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorCluster {
    /// The 32-bit Matter cluster id, e.g. `0xfff1fc01`. Upper 16 bits are the vendor.
    pub cluster_id: u32,
    /// The endpoint carrying it, which is how a user tells two apart on one device.
    pub endpoint: u16,
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
        /// The increment the device accepts, where it states one. A dishwasher
        /// taking 49 to 82 degrees in whole degrees will refuse 50.5.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        /// What this range is true of, where it is not true always.
        ///
        /// A thermostat's limits belong to whichever setpoint its mode has live, so
        /// the same device answers 7 to 23.5 while heating and 16 to 32 while
        /// cooling. Stated bare, the number reads as a fact about the device and
        /// goes stale the moment the mode changes — and it hides that the device
        /// reaches higher elsewhere. Absent for anything whose limits do not move.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when: Option<String>,
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

    /// Set a speaker's volume as a 0–100 percentage.
    ///
    /// Not brightness. Matter carries both on Level Control and the ENDPOINT's device
    /// type says whose level it is: a composed television has a speaker endpoint, and
    /// reporting its level as brightness offered a control that turned the sound down.
    async fn set_volume(&self, device_id: &str, _percent: u8) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' has no speaker to set a volume on")
    }

    /// Set colour temperature in kelvin — warm white to cool white.
    ///
    /// Separate from [`Self::set_color`] because it is a separate control, not a second
    /// way to reach the same one: 2700K white has no hue, so it cannot be asked for
    /// through hue and saturation at all. A device offers one, the other, or both, and
    /// `describe` says which.
    async fn set_color_temp(&self, device_id: &str, _kelvin: u32) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support colour temperature")
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

    /// Set a covering's slat angle, as a 0–100 percentage **open**.
    ///
    /// Separate from [`Self::set_position`] because they are separate axes: how far
    /// a blind is lowered and how far its slats are turned. A venetian blind is
    /// routinely down with its slats open, and position alone cannot ask for that.
    async fn set_tilt(&self, device_id: &str, _percent_open: u8) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support tilt")
    }

    /// Set a covering (blind/curtain/shade) position, or a valve's level, as a
    /// 0–100 percentage **open** — 100 is fully open, 0 fully closed.
    ///
    /// One verb for both because it is one axis: how far open the thing is. A valve
    /// that has no level says so and only [`Self::set_valve`] reaches it.
    async fn set_position(
        &self,
        device_id: &str,
        _percent_open: u8,
    ) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support position control")
    }

    /// Open or shut a valve.
    ///
    /// Separate from [`Self::set_power`] because a valve has no on/off switch to
    /// throw: Matter's Valve Configuration and Control takes `open` and `close`
    /// commands, and a device with only that cluster would answer a power request
    /// with "not supported" while sitting there perfectly openable. Separate from
    /// [`Self::set_position`] because a valve's level is optional — a plain solenoid
    /// is open or shut with nothing in between.
    async fn set_valve(&self, device_id: &str, _open: bool) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' has no valve to open or shut")
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

    /// A valve is not a plug with water in it.
    ///
    /// It has no on/off switch to throw -- Matter's Valve Configuration and Control
    /// takes `open` and `close` -- so a device with only that cluster answered every
    /// power request "not supported" while sitting there perfectly openable.
    #[tokio::test]
    async fn set_valve_records_and_echoes() {
        let dc = RecordingDeviceControl::default();

        let opened = dc.set_valve("garden-valve", true).await.unwrap();
        assert_eq!(opened.applied.valve, Some(true));
        assert_eq!(
            dc.last_call().as_deref(),
            Some("set_valve(garden-valve, open=true)")
        );

        let shut = dc.set_valve("garden-valve", false).await.unwrap();
        assert_eq!(shut.applied.valve, Some(false));
        // Not reported as a power change: nothing was switched.
        assert_eq!(shut.applied.on, None);
    }

    /// The opt-in default, so a backend that cannot open a valve says which device
    /// and why rather than reporting a success it did not perform.
    #[tokio::test]
    async fn a_backend_without_valves_says_so() {
        struct PowerOnly;

        #[async_trait]
        impl DeviceControlPort for PowerOnly {
            async fn set_power(&self, device_id: &str, _on: bool) -> Result<DeviceControlOutcome> {
                Ok(DeviceControlOutcome::new(
                    device_id,
                    DeviceStatePatch::default(),
                ))
            }
            async fn set_brightness(
                &self,
                device_id: &str,
                _percent: u8,
            ) -> Result<DeviceControlOutcome> {
                Ok(DeviceControlOutcome::new(
                    device_id,
                    DeviceStatePatch::default(),
                ))
            }
            async fn set_target_temp(
                &self,
                device_id: &str,
                _celsius: f32,
            ) -> Result<DeviceControlOutcome> {
                Ok(DeviceControlOutcome::new(
                    device_id,
                    DeviceStatePatch::default(),
                ))
            }
            async fn set_locked(
                &self,
                device_id: &str,
                _locked: bool,
            ) -> Result<DeviceControlOutcome> {
                Ok(DeviceControlOutcome::new(
                    device_id,
                    DeviceStatePatch::default(),
                ))
            }
        }

        let error = PowerOnly.set_valve("lamp-1", true).await.unwrap_err();
        assert!(
            error.to_string().contains("lamp-1") && error.to_string().contains("valve"),
            "{error}"
        );
    }
}
