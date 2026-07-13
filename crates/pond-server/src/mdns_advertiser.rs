//! mDNS service advertisement for LAN discovery.
//!
//! Registers a `_pond._tcp.local.` service so phones on the same network can
//! find this hub without manual IP entry.  The [`MdnsHandle`] keeps the
//! background `ServiceDaemon` alive; dropping it deregisters the service.

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};

/// Holds the running mDNS daemon. Drop to deregister.
pub struct MdnsHandle {
    daemon: ServiceDaemon,
    full_name: String,
}

impl Drop for MdnsHandle {
    fn drop(&mut self) {
        if let Err(e) = self.daemon.unregister(&self.full_name) {
            tracing::warn!("mDNS deregister failed: {e}");
        }
    }
}

/// Advertise `_pond._tcp.local.` on `port` using `hostname` as the instance label.
///
/// Returns [`None`] (with a warning log) rather than propagating the error, so
/// a missing mDNS stack never prevents the server from starting.
pub fn advertise(hostname: &str, port: u16, version: &str) -> Result<MdnsHandle> {
    let daemon = ServiceDaemon::new()?;

    let service_type = "_pond._tcp.local.";
    let instance_name = format!("Pond Hub @ {hostname}");
    let host_fqdn = format!("{hostname}.local.");

    let mut properties = std::collections::HashMap::new();
    properties.insert("v".to_string(), version.to_string());

    let service = ServiceInfo::new(
        service_type,
        &instance_name,
        &host_fqdn,
        "", // addresses filled in by enable_addr_auto() below
        port,
        Some(properties),
    )?
    // mdns-sd does NOT auto-populate interface addresses from an empty host —
    // without this the service advertises with no resolvable IP and phones
    // silently fail to connect. enable_addr_auto() makes the daemon announce
    // (and keep updated) every reachable interface address.
    .enable_addr_auto();

    let full_name = service.get_fullname().to_string();
    daemon.register(service)?;

    tracing::info!(
        hostname,
        port,
        "_pond._tcp.local. registered — phones on the same LAN can now discover this hub"
    );

    Ok(MdnsHandle { daemon, full_name })
}
