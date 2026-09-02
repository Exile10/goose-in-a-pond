//! Matter device backend (#195) — the first real [`DeviceControlPort`]
//! implementation, replacing the logging stub.
//!
//! GIAP talks to commissioned Matter devices (real or virtual) through a local
//! controller it installs and runs itself: `matter-server/`, a Node process
//! running [matter.js](https://github.com/matter-js/matter.js), reachable over
//! the `giap-matter` WebSocket protocol.
//!
//! ```text
//! GIAP chat / #92 rules ─► DeviceControlPort ─► MatterDeviceControl
//!                                                    │ ws://…:5580/giap
//!                                          matter-server (Node, the fabric)
//!                                                    │ encrypted Matter
//!                                     lights · locks · thermostats · sensors
//!
//! sensor changes ─► run_matter_bridge ─► BusEvent::Sensor ─► #92 rules
//! ```
//!
//! The protocol is domain-level: it carries devices, readings and control verbs
//! (`power`, `brightness`, `target_temp`, `locked`, `color`, `fan_speed`,
//! `fan_mode`, `position`), never endpoints, clusters or attribute paths. All
//! the Matter vocabulary — which cluster a verb becomes, what unit a value is
//! in, what device type a node claims — lives in the controller, where
//! matter.js's typed cluster models are. Nothing in this crate knows what a
//! cluster is. See `docs/matter-protocol.md`.
//!
//! Commissioning stays in the controller too (it owns the fabric credentials);
//! GIAP connects, syncs devices into its registry under stable
//! `matter-<node_id>` ids, and drives them. All traffic is LAN-local, and the
//! controller binds loopback only.
//!
//! This adapter is device-agnostic: real certified devices and virtual test
//! devices (e.g. Google's Matter Virtual Device app) are indistinguishable to
//! it. Devices are registered under their own announced identity (user label,
//! else vendor product name, else "Light N"), and users address them naturally
//! in chat ("turn off the light") via the device-control tool's name
//! resolution.

mod bridge;
mod client;
mod commissioning;
mod control;
mod notify;
mod protocol;
mod runtime;
mod server_setup;

pub use bridge::{run_matter_bridge, run_matter_supervisor, SupervisorConfig};
pub use client::{code_of, MatterClient, MatterEvent};
pub use commissioning::MatterCommissioner;
pub use control::{MatterDeviceControl, SharedMatterClient};
pub use notify::MatterNotifier;
pub use protocol::{
    is_matter_device_id, matter_bridged_endpoint, matter_device_id, matter_node_id,
    redact_setup_code, WireDevice, WireReading,
};
pub use runtime::{MatterRuntime, SwitchableDeviceControl};
pub use server_setup::{
    ensure_running as ensure_matter_server, local_port_from_ws_url, revive_local_controller,
    Revival, SharedServerChild,
};

#[cfg(test)]
mod tests;
