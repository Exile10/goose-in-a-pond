//! Which addresses the pond asks for when it looks a name up.
//!
//! A lookup asks for IPv4 and IPv6 addresses together, and waits for both answers. Some home
//! routers never answer the IPv6 question for a name that has no IPv6 address: on the Orin's
//! network (2026-10-12) every lookup of `api.open-meteo.com` and `credentials.jarida.io` waited
//! 15 s for that answer, and the weather tool, which allows 10 s in all, failed every time.
//!
//! A device with no route to the IPv6 internet could not use an IPv6 address anyway, so on such
//! a device the pond asks for IPv4 addresses only, and the question that stalls is never sent.
//! Nothing else changes: the same system resolver answers, so `/etc/hosts`, `.local` names and
//! the household's DNS server all apply as before. With an IPv6 route, lookups are exactly
//! reqwest's default. The one cost: a service listening only on `::1` is not reachable by name
//! from an IPv4-only pond.

use std::ffi::{CStr, CString};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::OnceLock;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// The error type reqwest's `Resolving` carries.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Decided once per process, on first use; [`decide_at_startup`] makes that first use startup.
static IPV4_ONLY: OnceLock<bool> = OnceLock::new();

/// Whether this process looks up IPv4 addresses only: true when the device has no IPv6 route.
pub fn ipv4_only() -> bool {
    *IPV4_ONLY.get_or_init(|| !has_ipv6_route())
}

/// Decide now, at startup, rather than inside the first request, and put the decision on
/// record. Call it after logging starts.
pub fn decide_at_startup() {
    if ipv4_only() {
        tracing::info!(
            kind = "dns_family",
            family = "ipv4",
            "this device has no route to the IPv6 internet, so the pond looks up IPv4 addresses \
             only: a resolver that never answers IPv6 lookups cannot stall it"
        );
    } else {
        tracing::debug!(
            kind = "dns_family",
            family = "any",
            "this device has an IPv6 route; the pond looks up IPv4 and IPv6 addresses"
        );
    }
}

/// Whether the kernel has a route to the IPv6 internet. A UDP `connect` only consults the
/// routing table, so this sends nothing; the address is Google Public DNS's, never contacted.
fn has_ipv6_route() -> bool {
    UdpSocket::bind("[::]:0")
        .and_then(|socket| socket.connect("[2001:4860:4860::8888]:53"))
        .is_ok()
}

/// The resolver every pond HTTP client uses (see [`crate::client_builder`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct PondResolver;

impl Resolve for PondResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let ipv4_only = ipv4_only();
        Box::pin(async move {
            // getaddrinfo blocks, as in reqwest's own resolver.
            let addrs = tokio::task::spawn_blocking(move || lookup(&host, ipv4_only)).await??;
            Ok::<Addrs, BoxError>(Box::new(addrs.into_iter()))
        })
    }
}

/// `host`'s addresses, port 0 (the client sets the port). With `ipv4_only`, only the IPv4
/// question is asked; otherwise this is what reqwest's default resolver does.
pub fn lookup(host: &str, ipv4_only: bool) -> io::Result<Vec<SocketAddr>> {
    if ipv4_only {
        lookup_ipv4(host)
    } else {
        Ok((host, 0).to_socket_addrs()?.collect())
    }
}

/// `getaddrinfo` asked for `AF_INET` only, so no AAAA query is sent.
#[allow(unsafe_code)]
fn lookup_ipv4(host: &str) -> io::Result<Vec<SocketAddr>> {
    let c_host = CString::new(host).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a host name cannot contain a NUL byte",
        )
    })?;
    // SAFETY: all zero is a valid `addrinfo`, meaning no hints; family and type are set below.
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_INET;
    hints.ai_socktype = libc::SOCK_STREAM;
    let mut list: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: a NUL-terminated host, no service, valid hints, and an out-pointer the call fills.
    let code = unsafe { libc::getaddrinfo(c_host.as_ptr(), std::ptr::null(), &hints, &mut list) };
    if code != 0 {
        return Err(lookup_error(code));
    }
    let mut addrs = Vec::new();
    let mut entry = list;
    while !entry.is_null() {
        // SAFETY: `entry` is a node of the list getaddrinfo returned, not yet freed.
        let info = unsafe { &*entry };
        if info.ai_family == libc::AF_INET && !info.ai_addr.is_null() {
            // SAFETY: an AF_INET entry's address is a `sockaddr_in`.
            let sin = unsafe { &*(info.ai_addr as *const libc::sockaddr_in) };
            let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
            addrs.push(SocketAddr::new(IpAddr::V4(ip), 0));
        }
        entry = info.ai_next;
    }
    // SAFETY: `list` came from a successful getaddrinfo and is freed once, here.
    unsafe { libc::freeaddrinfo(list) };
    Ok(addrs)
}

/// The error std's own lookup gives for the same failure.
#[allow(unsafe_code)]
fn lookup_error(code: libc::c_int) -> io::Error {
    if code == libc::EAI_SYSTEM {
        return io::Error::last_os_error();
    }
    // SAFETY: gai_strerror returns a pointer to a static NUL-terminated message.
    let message = unsafe { CStr::from_ptr(libc::gai_strerror(code)) };
    io::Error::other(format!(
        "failed to lookup address information: {}",
        message.to_string_lossy()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ipv4_only_lookup_returns_only_ipv4_addresses() {
        let addrs = lookup("localhost", true).unwrap();
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(SocketAddr::is_ipv4), "{addrs:?}");
        assert!(
            addrs.contains(&SocketAddr::from(([127, 0, 0, 1], 0))),
            "{addrs:?}"
        );
    }

    #[test]
    fn an_ip_literal_is_its_own_address() {
        assert_eq!(
            lookup("192.0.2.7", true).unwrap(),
            vec![SocketAddr::from(([192, 0, 2, 7], 0))]
        );
    }

    #[test]
    fn the_default_lookup_is_reqwests() {
        let ours = lookup("localhost", false).unwrap();
        let std: Vec<SocketAddr> = ("localhost", 0).to_socket_addrs().unwrap().collect();
        assert_eq!(ours, std);
    }

    #[test]
    fn a_name_that_cannot_be_a_host_is_refused_not_looked_up() {
        let err = lookup("bad\0name", true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// Whatever this machine's network, the probe answers without sending or hanging.
    #[test]
    fn the_route_probe_answers() {
        let _ = has_ipv6_route();
    }

    #[tokio::test]
    async fn the_resolver_answers_for_a_name_on_this_machine() {
        let addrs: Vec<SocketAddr> = PondResolver
            .resolve("localhost".parse().unwrap())
            .await
            .unwrap()
            .collect();
        assert!(!addrs.is_empty());
        if ipv4_only() {
            assert!(addrs.iter().all(SocketAddr::is_ipv4), "{addrs:?}");
        }
    }
}
