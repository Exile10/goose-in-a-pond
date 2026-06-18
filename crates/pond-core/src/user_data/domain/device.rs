//! Device-control domain types (#84).
//!
//! Protocol-agnostic, *typed* representations of a controllable device's
//! capabilities and state. There is deliberately no opaque `String`/JSON state
//! blob: every value carries its type via [`DeviceStateValue`], so adapters and
//! the rules engine reason about state without parsing untyped text (which also
//! removes a string-injection vector at the control boundary).
//!
//! Pure domain — no `tokio`, `sqlx`, `reqwest`, or other framework imports.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Stable identifier for a controllable device.
///
/// A newtype rather than a bare `String` so a device id can't be silently
/// confused with any other string (sensor id, session id, …) at a call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for DeviceId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for DeviceId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// A capability a device advertises. Adapters map their protocol's features
/// onto this closed set so the Core can negotiate control generically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCapability {
    /// On/off switching (the `power` state key).
    Power,
    /// Brightness, 0–100 (the `brightness` state key, `Int`).
    Dimmable,
    /// White color temperature in Kelvin (the `color_temp_k` key, `Int`).
    ColorTemperature,
    /// RGB color (the `rgb` key, `Text` as `#rrggbb`).
    Rgb,
    /// Target temperature in °C (the `target_temp_c` key, `Float`).
    Thermostat,
    /// Lock/unlock (the `locked` state key, `Bool`).
    Lock,
    /// Fan speed, 0–100 (the `fan_speed` key, `Int`).
    FanSpeed,
}

/// A single typed state value. The closed variant set keeps state values
/// machine-checkable end-to-end (no opaque strings to parse or inject through).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStateValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl DeviceStateValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }
}

/// The canonical state-map key for on/off power.
pub const POWER_KEY: &str = "power";

/// A device's full key→value state. Backed by a `BTreeMap` for deterministic
/// ordering (stable serialization, reproducible test assertions).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceState {
    pub values: BTreeMap<String, DeviceStateValue>,
}

impl DeviceState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&DeviceStateValue> {
        self.values.get(key)
    }

    pub fn set(&mut self, key: impl Into<String>, value: DeviceStateValue) {
        self.values.insert(key.into(), value);
    }

    /// Convenience accessor for the canonical power key.
    pub fn power(&self) -> Option<bool> {
        self.get(POWER_KEY).and_then(DeviceStateValue::as_bool)
    }
}

/// Emitted on the [`crate::shared::ports::event_bus::EventBus`] when a device's state
/// changes, so reactive consumers (e.g. the rules engine) can respond.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceStateChanged {
    pub device_id: DeviceId,
    pub key: String,
    pub value: DeviceStateValue,
    pub changed_at: DateTime<Utc>,
}
