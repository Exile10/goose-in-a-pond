//! The host's wired network interfaces for each OS, behind `pond_core`'s `WiredInterfaces` port.

use std::sync::Arc;

use async_trait::async_trait;
use pond_core::network::domain::wired::WiredInterface;
use pond_core::network::ports::wired_interfaces::WiredInterfaces;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(any(target_os = "linux", test))]
pub mod sysfs;

/// This host's reader: sysfs on Linux, getifaddrs and the interface medium on macOS.
pub fn platform() -> Arc<dyn WiredInterfaces> {
    #[cfg(target_os = "linux")]
    return Arc::new(sysfs::SysfsWired::host());
    #[cfg(target_os = "macos")]
    return Arc::new(macos::MacWired);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Arc::new(NoWiredInterfaces);
}

/// A host with no reader for its OS shows no wired interface rather than a guessed one.
pub struct NoWiredInterfaces;

#[async_trait]
impl WiredInterfaces for NoWiredInterfaces {
    async fn wired_interfaces(&self) -> Vec<WiredInterface> {
        Vec::new()
    }
}

/// The first address a phone could use: not loopback, not link-local.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn first_address(
    addresses: &[(String, std::net::Ipv4Addr)],
    interface: &str,
) -> Option<std::net::Ipv4Addr> {
    addresses
        .iter()
        .find(|(name, ip)| name == interface && !ip.is_loopback() && !ip.is_link_local())
        .map(|(_, ip)| *ip)
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[tokio::test]
    async fn an_unknown_os_shows_nothing() {
        assert!(NoWiredInterfaces.wired_interfaces().await.is_empty());
    }

    #[test]
    fn the_first_usable_address_of_that_interface_is_shown() {
        let addresses = [
            ("eth0".to_string(), Ipv4Addr::new(169, 254, 3, 4)),
            ("eth1".to_string(), Ipv4Addr::new(10, 0, 0, 9)),
            ("eth0".to_string(), Ipv4Addr::new(192, 168, 1, 20)),
            ("eth0".to_string(), Ipv4Addr::new(192, 168, 1, 21)),
        ];
        assert_eq!(
            first_address(&addresses, "eth0"),
            Some(Ipv4Addr::new(192, 168, 1, 20))
        );
        assert_eq!(first_address(&addresses, "eth2"), None);
    }
}
