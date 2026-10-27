//! The Mac's wired interfaces. Every Wi-Fi interface (`en0`, `awdl0`, `llw0`, `ap1`) reports
//! Ethernet framing too, so the medium tells them apart; and macOS keeps `IFF_RUNNING` set on a
//! port with no cable, so the link is read from the medium's status, as `ifconfig` reads it.

use std::ffi::{c_char, c_int, CStr};
use std::io;
use std::net::Ipv4Addr;

use async_trait::async_trait;
use pond_core::network::domain::wired::{LinkState, WiredInterface};
use pond_core::network::ports::wired_interfaces::WiredInterfaces;

use crate::interfaces;

/// `<net/if_types.h>`.
const IFT_ETHER: u8 = 0x06;
const IFNAMSIZ: usize = 16;
/// `_IOWR('i', 56, struct ifmediareq)`, from `<sys/sockio.h>`.
const SIOCGIFMEDIA: libc::c_ulong = 0xc02c_6938;
/// `<net/if_media.h>`.
const IFM_NMASK: c_int = 0xe0;
const IFM_ETHER: c_int = 0x20;
const IFM_TMASK: c_int = 0x1f;
const IFM_NONE: c_int = 0x02;
const IFM_AVALID: c_int = 0x01;
const IFM_ACTIVE: c_int = 0x02;
const BITS_PER_MEGABIT: u32 = 1_000_000;

/// `struct ifmediareq`, which `<net/if.h>` declares under `#pragma pack(4)`.
#[repr(C, packed(4))]
struct IfMediaReq {
    name: [c_char; IFNAMSIZ],
    current: c_int,
    mask: c_int,
    status: c_int,
    active: c_int,
    count: c_int,
    list: *mut c_int,
}

const _: () = assert!(std::mem::size_of::<IfMediaReq>() == 44);

pub struct MacWired;

#[async_trait]
impl WiredInterfaces for MacWired {
    async fn wired_interfaces(&self) -> Vec<WiredInterface> {
        match read() {
            Ok(wired) => wired,
            Err(error) => {
                tracing::warn!(%error, "cannot read this Mac's interfaces; showing no wired ones");
                Vec::new()
            }
        }
    }
}

/// What one Ethernet-framed interface's medium says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Medium {
    current: c_int,
    status: c_int,
}

impl Medium {
    /// An Ethernet medium that can carry a cable. Apple's internal USB-C stubs (`anpi0`, the
    /// "Ethernet Adapter" ports with nothing behind them) report no medium at all.
    fn is_wired(self) -> bool {
        self.current & IFM_NMASK == IFM_ETHER && self.current & IFM_TMASK != IFM_NONE
    }

    fn link(self) -> LinkState {
        if self.status & IFM_AVALID != 0 && self.status & IFM_ACTIVE != 0 {
            LinkState::Up
        } else {
            LinkState::Down
        }
    }
}

fn read() -> io::Result<Vec<WiredInterface>> {
    let links = ethernet_links()?;
    let addresses = interfaces::ipv4_addresses().unwrap_or_else(|error| {
        tracing::warn!(%error, "cannot read this Mac's addresses; wired ones show none");
        Vec::new()
    });
    let socket = Socket::open()?;
    let mut wired: Vec<WiredInterface> = links
        .into_iter()
        .filter_map(|(name, baudrate)| {
            let medium = socket.medium(&name)?;
            medium
                .is_wired()
                .then(|| wired_interface(name, medium, baudrate, &addresses))
        })
        .collect();
    wired.sort_by(|a, b| a.interface.cmp(&b.interface));
    Ok(wired)
}

fn wired_interface(
    interface: String,
    medium: Medium,
    baudrate: u32,
    addresses: &[(String, Ipv4Addr)],
) -> WiredInterface {
    let link = medium.link();
    WiredInterface {
        address: super::first_address(addresses, &interface),
        speed_mbps: Some(baudrate / BITS_PER_MEGABIT)
            .filter(|speed| link == LinkState::Up && *speed > 0),
        link,
        interface,
    }
}

/// Every Ethernet-framed interface that is neither loopback nor point-to-point, with its baud rate.
fn ethernet_links() -> io::Result<Vec<(String, u32)>> {
    struct List(*mut libc::ifaddrs);
    impl Drop for List {
        fn drop(&mut self) {
            // SAFETY: the pointer came from a successful getifaddrs and is freed exactly once.
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let mut head = std::mem::MaybeUninit::<*mut libc::ifaddrs>::uninit();
    // SAFETY: getifaddrs writes the head of a list libc owns when it returns 0.
    if unsafe { libc::getifaddrs(head.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: written by the successful call above; null means no interfaces at all.
    let list = List(unsafe { head.assume_init() });
    let mut links = Vec::new();
    let mut current = std::ptr::NonNull::new(list.0);
    while let Some(entry) = current {
        // SAFETY: non-null, and every entry stays allocated until `list` drops after the walk.
        let entry = unsafe { entry.as_ref() };
        current = std::ptr::NonNull::new(entry.ifa_next);
        let flags = entry.ifa_flags as c_int;
        if flags & (libc::IFF_LOOPBACK | libc::IFF_POINTOPOINT) != 0 {
            continue;
        }
        let (Some(addr), Some(data), Some(name)) = (
            std::ptr::NonNull::new(entry.ifa_addr),
            std::ptr::NonNull::new(entry.ifa_data),
            std::ptr::NonNull::new(entry.ifa_name),
        ) else {
            continue;
        };
        // SAFETY: a non-null ifa_addr points at a sockaddr of at least sa_family's size.
        if c_int::from(unsafe { addr.as_ref() }.sa_family) != libc::AF_LINK {
            continue;
        }
        // SAFETY: an AF_LINK entry's ifa_data is the interface's `struct if_data`.
        let data = unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<libc::if_data>()) };
        if data.ifi_type != IFT_ETHER {
            continue;
        }
        // SAFETY: getifaddrs guarantees a nul-terminated interface name.
        let name = unsafe { CStr::from_ptr(name.as_ptr()) };
        links.push((name.to_string_lossy().into_owned(), data.ifi_baudrate));
    }
    Ok(links)
}

/// A datagram socket, only for the media ioctl.
struct Socket(c_int);

impl Socket {
    fn open() -> io::Result<Self> {
        // SAFETY: a plain socket(2) call; the descriptor is closed in Drop.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(fd))
    }

    /// `None` for an interface with no medium to report (a bridge, a tunnel).
    fn medium(&self, name: &str) -> Option<Medium> {
        if name.len() >= IFNAMSIZ {
            return None;
        }
        let mut request = IfMediaReq {
            name: [0; IFNAMSIZ],
            current: 0,
            mask: 0,
            status: 0,
            active: 0,
            count: 0,
            list: std::ptr::null_mut(),
        };
        for (slot, byte) in request.name.iter_mut().zip(name.bytes()) {
            *slot = byte as c_char;
        }
        // SAFETY: SIOCGIFMEDIA fills `request`, whose layout is `struct ifmediareq`; with a null
        // list and a zero count the kernel writes no media list.
        let answered = unsafe { libc::ioctl(self.0, SIOCGIFMEDIA, &mut request) } == 0;
        answered.then_some(Medium {
            current: request.current,
            status: request.status,
        })
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // SAFETY: the descriptor came from a successful socket(2) and is closed exactly once.
        unsafe { libc::close(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values this probe read from a MacBook's interfaces with `SIOCGIFMEDIA`.
    #[test]
    fn the_medium_tells_a_cable_port_from_wifi_and_from_a_stub() {
        let thunderbolt = Medium {
            current: 0x100020,
            status: IFM_AVALID,
        };
        assert!(thunderbolt.is_wired());
        assert_eq!(thunderbolt.link(), LinkState::Down, "no cable");
        let wifi = Medium {
            current: 0x80,
            status: IFM_AVALID | IFM_ACTIVE,
        };
        assert!(!wifi.is_wired());
        let stub = Medium {
            current: 0x22,
            status: IFM_AVALID,
        };
        assert!(!stub.is_wired());
        let plugged = Medium {
            current: 0x20,
            status: IFM_AVALID | IFM_ACTIVE,
        };
        assert_eq!(plugged.link(), LinkState::Up);
        assert_eq!(
            Medium {
                current: 0x20,
                status: IFM_ACTIVE
            }
            .link(),
            LinkState::Down,
            "an invalid status says nothing"
        );
    }

    #[test]
    fn speed_is_shown_only_for_a_live_link() {
        let addresses = [("en5".to_string(), Ipv4Addr::new(192, 168, 1, 30))];
        let up = Medium {
            current: 0x20,
            status: IFM_AVALID | IFM_ACTIVE,
        };
        let live = wired_interface("en5".into(), up, 1_000_000_000, &addresses);
        assert_eq!(
            live,
            WiredInterface {
                interface: "en5".into(),
                link: LinkState::Up,
                address: Some(Ipv4Addr::new(192, 168, 1, 30)),
                speed_mbps: Some(1000),
            }
        );
        let down = Medium {
            current: 0x20,
            status: IFM_AVALID,
        };
        assert_eq!(
            wired_interface("en5".into(), down, 1_000_000_000, &addresses).speed_mbps,
            None
        );
        assert_eq!(
            wired_interface("en5".into(), up, 0, &addresses).speed_mbps,
            None
        );
    }

    /// Walks this Mac's real interfaces: the radios every Mac has never show up as wired.
    #[test]
    fn this_macs_wireless_and_virtual_interfaces_are_never_wired() {
        let wired = read().expect("getifaddrs");
        for name in ["awdl0", "llw0", "ap1", "lo0", "bridge0", "utun0"] {
            assert!(
                wired.iter().all(|w| w.interface != name),
                "{name} listed as wired: {wired:?}"
            );
        }
    }
}
