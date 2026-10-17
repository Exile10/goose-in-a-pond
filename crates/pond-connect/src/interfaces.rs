//! The host's IPv4 addresses, read live, for the advertising decision and a Wi-Fi join's wait for
//! an address. The judgement of which ones count is pond-core's.

use std::io;
use std::net::Ipv4Addr;

use pond_core::network::domain::interface;

/// Whether the pond has a network a phone could reach it on; a seam so the advertising
/// controller is tested without the host's own interfaces.
pub trait NetworkProbe: Send + Sync {
    fn has_usable_network(&self) -> bool;
}

/// This host's interfaces.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostNetwork;

impl NetworkProbe for HostNetwork {
    fn has_usable_network(&self) -> bool {
        match ipv4_addresses() {
            Ok(addresses) => interface::has_usable_network(
                addresses.iter().map(|(name, ip)| (name.as_str(), *ip)),
            ),
            Err(error) => {
                // Read as offline: that only ever offers more setup, never less.
                tracing::warn!(%error, "could not read this host's addresses; treating it as offline");
                false
            }
        }
    }
}

/// Every IPv4 address on an interface that is up and not point-to-point, with its interface name.
#[cfg(unix)]
pub fn ipv4_addresses() -> io::Result<Vec<(String, Ipv4Addr)>> {
    struct List(*mut libc::ifaddrs);
    impl Drop for List {
        fn drop(&mut self) {
            // SAFETY: the pointer came from a successful getifaddrs and is freed exactly once.
            unsafe { libc::freeifaddrs(self.0) };
        }
    }

    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs writes the head of a list libc owns when it returns 0.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let list = List(head);
    let mut addresses = Vec::new();
    let mut cursor = list.0;
    while !cursor.is_null() {
        // SAFETY: non-null, and every entry stays allocated until `list` drops.
        let entry = unsafe { &*cursor };
        cursor = entry.ifa_next;
        let flags = entry.ifa_flags as libc::c_int;
        if flags & libc::IFF_UP == 0
            || flags & libc::IFF_POINTOPOINT != 0
            || entry.ifa_addr.is_null()
            || entry.ifa_name.is_null()
        {
            continue;
        }
        // SAFETY: ifa_addr is non-null and points at a sockaddr of at least sa_family's size.
        let family = libc::c_int::from(unsafe { (*entry.ifa_addr).sa_family });
        if family != libc::AF_INET {
            continue;
        }
        // SAFETY: an AF_INET entry's address is a sockaddr_in; read unaligned to assume nothing.
        let address =
            unsafe { std::ptr::read_unaligned(entry.ifa_addr as *const libc::sockaddr_in) };
        // SAFETY: getifaddrs guarantees a nul-terminated interface name.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }
            .to_string_lossy()
            .into_owned();
        addresses.push((name, Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr))));
    }
    Ok(addresses)
}

#[cfg(not(unix))]
pub fn ipv4_addresses() -> io::Result<Vec<(String, Ipv4Addr)>> {
    Ok(Vec::new())
}

/// The first IPv4 address `interface_name` holds now that is not link-local.
pub fn ipv4_on(interface_name: &str) -> Option<Ipv4Addr> {
    ipv4_addresses()
        .ok()?
        .into_iter()
        .find(|(name, ip)| name == interface_name && !ip.is_link_local() && !ip.is_loopback())
        .map(|(_, ip)| ip)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_walk_reaches_loopback() {
        let addresses = ipv4_addresses().expect("getifaddrs");
        assert!(
            addresses.iter().any(|(_, ip)| ip.is_loopback()),
            "no loopback among {addresses:?}"
        );
        assert_eq!(
            ipv4_on("lo0").or(ipv4_on("lo")),
            None,
            "loopback is never a network"
        );
    }
}
