use pond_core::domain::device::{CapabilityKind, DeviceCapability};
use std::collections::HashMap;

/// Maps canonical GIAP capability key names to standard LIRC key names.
///
/// Key resolution order when `set_state(key, value)` is called:
///  1. Exact match in this table  →  use the mapped LIRC key
///  2. No match  →  use `key` directly as the LIRC key name
///
/// This lets advanced users store exact LIRC key names in their device's
/// capability keys (e.g. `KEY_HDMI1`) while common ones (e.g. `volume_up`)
/// just work out of the box.
pub fn default_key_map() -> HashMap<&'static str, &'static str> {
    [
        // Power
        ("power",          "KEY_POWER"),
        // Volume
        ("volume_up",      "KEY_VOLUMEUP"),
        ("volume_down",    "KEY_VOLUMEDOWN"),
        ("mute",           "KEY_MUTE"),
        // Channels / navigation
        ("channel_up",     "KEY_CHANNELUP"),
        ("channel_down",   "KEY_CHANNELDOWN"),
        ("up",             "KEY_UP"),
        ("down",           "KEY_DOWN"),
        ("left",           "KEY_LEFT"),
        ("right",          "KEY_RIGHT"),
        ("ok",             "KEY_OK"),
        ("enter",          "KEY_ENTER"),
        ("back",           "KEY_BACK"),
        ("menu",           "KEY_MENU"),
        ("home",           "KEY_HOME"),
        ("exit",           "KEY_EXIT"),
        // Inputs
        ("input",          "KEY_INPUT"),
        ("source",         "KEY_SOURCE"),
        ("hdmi1",          "KEY_HDMI1"),
        ("hdmi2",          "KEY_HDMI2"),
        ("hdmi3",          "KEY_HDMI3"),
        ("av",             "KEY_AV"),
        // Media
        ("play",           "KEY_PLAY"),
        ("pause",          "KEY_PAUSE"),
        ("stop",           "KEY_STOP"),
        ("rewind",         "KEY_REWIND"),
        ("fast_forward",   "KEY_FASTFORWARD"),
        ("next",           "KEY_NEXT"),
        ("previous",       "KEY_PREVIOUS"),
        // Display / picture
        ("brightness_up",  "KEY_BRIGHTNESSUP"),
        ("brightness_down","KEY_BRIGHTNESSDOWN"),
        // AC / climate (common among IR ACs)
        ("mode",           "KEY_MODE"),
        ("temp_up",        "KEY_UP"),    // many ACs reuse nav keys in AC mode
        ("temp_down",      "KEY_DOWN"),
        ("fan_speed",      "KEY_FAN"),
        ("swing",          "KEY_PROGRAM"),
        ("sleep",          "KEY_SLEEP"),
    ]
    .into_iter()
    .collect()
}

/// Starter structured capabilities for a generic IR-controlled TV.
///
/// Attach these to a device's `structured_capabilities` field when registering
/// a TV so that the MCP control tools can enumerate what it supports.
pub fn tv_capabilities() -> Vec<DeviceCapability> {
    vec![
        DeviceCapability {
            key: "power".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "volume_up".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "volume_down".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "mute".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "input".into(),
            kind: CapabilityKind::Enum {
                options: vec!["hdmi1".into(), "hdmi2".into(), "hdmi3".into(), "av".into()],
            },
        },
        DeviceCapability {
            key: "channel_up".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "channel_down".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "menu".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "back".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "home".into(),
            kind: CapabilityKind::Toggle,
        },
    ]
}

/// Starter structured capabilities for a generic IR-controlled AC unit.
pub fn ac_capabilities() -> Vec<DeviceCapability> {
    vec![
        DeviceCapability {
            key: "power".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "mode".into(),
            kind: CapabilityKind::Enum {
                options: vec!["cool".into(), "heat".into(), "fan".into(), "dry".into(), "auto".into()],
            },
        },
        DeviceCapability {
            key: "temp_up".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "temp_down".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "fan_speed".into(),
            kind: CapabilityKind::Enum {
                options: vec!["low".into(), "medium".into(), "high".into(), "auto".into()],
            },
        },
        DeviceCapability {
            key: "sleep".into(),
            kind: CapabilityKind::Toggle,
        },
        DeviceCapability {
            key: "swing".into(),
            kind: CapabilityKind::Toggle,
        },
    ]
}
