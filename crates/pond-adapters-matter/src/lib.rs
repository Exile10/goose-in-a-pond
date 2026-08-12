//! Matter device backend (#195) — the first real [`DeviceControlPort`]
//! implementation, replacing the logging stub. GIAP talks to commissioned
//! Matter devices (real or virtual) through a local
//! [python-matter-server](https://github.com/home-assistant-libs/python-matter-server)
//! controller's WebSocket API:
//!
//! ```text
//! GIAP chat / #92 rules ─► DeviceControlPort ─► MatterDeviceControl
//!                                                    │ ws://…:5580/ws
//!                                        python-matter-server (fabric)
//!                                                    │ encrypted Matter
//!                                     lights · locks · thermostats · sensors
//!
//! sensor attribute updates ─► run_matter_bridge ─► BusEvent::Sensor ─► #92 rules
//! ```
//!
//! Cluster mapping (v1): OnOff (6) → `set_power`, LevelControl (8) →
//! `set_brightness`, Thermostat (513) setpoint → `set_target_temp`, DoorLock
//! (257) → `set_locked`; Occupancy / BooleanState / Temperature / Humidity
//! updates become sensor readings. Color, fan, and covering support are
//! follow-up `DeviceControlPort` verbs.
//!
//! Commissioning stays in the controller (it owns the fabric credentials);
//! GIAP connects, syncs nodes into its device registry under stable
//! `matter-<node_id>` ids, and commands them. All traffic is LAN-local.
//!
//! This adapter is device-agnostic: it speaks Matter clusters, so real
//! certified devices and virtual test devices (e.g. Google's Matter Virtual
//! Device app) are indistinguishable to it — a virtual light is simply a
//! Matter light. Devices are registered under their own announced identity
//! (user label, else vendor product name, else "Light N"), and users address
//! them naturally in chat ("turn off the light") via the device-control
//! tool's name resolution.

mod bridge;
mod client;
mod commissioning;
mod control;
mod protocol;
mod runtime;
mod server_setup;

pub use bridge::{run_matter_bridge, run_matter_supervisor, SupervisorConfig};
pub use client::{MatterClient, MatterEvent};
pub use commissioning::MatterCommissioner;
pub use control::{MatterDeviceControl, NodeCache, SharedMatterClient};
pub use protocol::{device_id_for_node, node_id_from_device_id, node_to_device, MatterNode};
pub use runtime::{MatterRuntime, SwitchableDeviceControl};
pub use server_setup::{
    ensure_running as ensure_matter_server, local_port_from_ws_url, revive_local_controller,
    Revival, SharedServerChild,
};

#[cfg(test)]
mod tests;
