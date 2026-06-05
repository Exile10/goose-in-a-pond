use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A typed value held in device state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StateValue {
    Bool(bool),
    Number(f64),
    Text(String),
}

/// Describes what kind of control a capability supports.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CapabilityKind {
    /// On/off toggle (maps to `set_power` or a bool state key).
    Toggle,
    /// Numeric value within bounds. `step` is the minimum increment if applicable.
    Range { min: f64, max: f64, step: Option<f64> },
    /// One of a fixed set of named modes (e.g. "heat", "cool", "fan").
    Enum { options: Vec<String> },
    /// Readable but not writable via this port.
    ReadOnly,
}

/// A single capability exposed by a device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCapability {
    /// Protocol-agnostic key used in `set_state` / `query_state` (e.g. "brightness", "mode").
    pub key: String,
    pub kind: CapabilityKind,
}

/// Current state snapshot for one device.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceState {
    pub device_id: String,
    pub values: HashMap<String, StateValue>,
}
