//! Linux's wired interfaces from `/sys/class/net`: an Ethernet-type device that is neither
//! wireless nor virtual (bridges, veth pairs, docker0, the Jetson's `l4tbr0`).

use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use pond_core::network::domain::wired::{LinkState, WiredInterface};
use pond_core::network::ports::wired_interfaces::WiredInterfaces;

const SYS_CLASS_NET: &str = "/sys/class/net";
/// `ARPHRD_ETHER` in `<linux/if_arp.h>`: Wi-Fi reports it too, hence the wireless checks.
const ARPHRD_ETHER: &str = "1";
const VIRTUAL_DEVICES: &str = "/devices/virtual/";

/// Where the IPv4 addresses come from, so a test can supply its own.
pub type AddressSource = fn() -> io::Result<Vec<(String, Ipv4Addr)>>;

pub struct SysfsWired {
    root: PathBuf,
    addresses: AddressSource,
}

impl SysfsWired {
    pub fn host() -> Self {
        Self::new(
            PathBuf::from(SYS_CLASS_NET),
            crate::interfaces::ipv4_addresses,
        )
    }

    pub fn new(root: PathBuf, addresses: AddressSource) -> Self {
        Self { root, addresses }
    }

    /// Reads sysfs, which lives in memory, so it is cheap enough to do on the caller's task.
    pub fn read(&self) -> Vec<WiredInterface> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(%error, root = %self.root.display(), "cannot list network interfaces; showing no wired ones");
                return Vec::new();
            }
        };
        let addresses = (self.addresses)().unwrap_or_else(|error| {
            tracing::warn!(%error, "cannot read this host's addresses; wired ones show none");
            Vec::new()
        });
        let mut wired: Vec<WiredInterface> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let interface = entry.file_name().into_string().ok()?;
                let dir = entry.path();
                if !is_wired(&dir) {
                    return None;
                }
                let link = link(&dir);
                Some(WiredInterface {
                    address: super::first_address(&addresses, &interface),
                    speed_mbps: (link == LinkState::Up).then(|| speed(&dir)).flatten(),
                    link,
                    interface,
                })
            })
            .collect();
        wired.sort_by(|a, b| a.interface.cmp(&b.interface));
        wired
    }
}

#[async_trait]
impl WiredInterfaces for SysfsWired {
    async fn wired_interfaces(&self) -> Vec<WiredInterface> {
        self.read()
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

/// `phy80211` as well as `wireless`: a kernel without wireless extensions has only the first.
fn is_wired(dir: &Path) -> bool {
    read_trimmed(&dir.join("type")).as_deref() == Some(ARPHRD_ETHER)
        && !dir.join("wireless").exists()
        && !dir.join("phy80211").exists()
        && fs::canonicalize(dir).is_ok_and(|real| !real.to_string_lossy().contains(VIRTUAL_DEVICES))
}

/// Some drivers never leave `unknown`; their carrier still says whether a cable is live.
fn link(dir: &Path) -> LinkState {
    match read_trimmed(&dir.join("operstate")).as_deref() {
        Some("up") => LinkState::Up,
        Some("unknown") if read_trimmed(&dir.join("carrier")).as_deref() == Some("1") => {
            LinkState::Up
        }
        _ => LinkState::Down,
    }
}

/// Megabits per second; the kernel writes -1 or fails the read when it does not know.
fn speed(dir: &Path) -> Option<u32> {
    read_trimmed(&dir.join("speed"))?
        .parse::<i64>()
        .ok()
        .filter(|speed| *speed > 0)
        .and_then(|speed| u32::try_from(speed).ok())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn addresses() -> io::Result<Vec<(String, Ipv4Addr)>> {
        Ok(vec![
            ("wlan0".into(), Ipv4Addr::new(192, 168, 1, 11)),
            ("eth0".into(), Ipv4Addr::new(192, 168, 1, 20)),
            ("enp3s0".into(), Ipv4Addr::new(169, 254, 3, 4)),
        ])
    }

    fn unreadable() -> io::Result<Vec<(String, Ipv4Addr)>> {
        Err(io::Error::other("getifaddrs failed"))
    }

    /// A device under `devices/<parent>/net/<name>`, linked from `class/net/<name>` as sysfs does.
    fn device(root: &Path, parent: &str, name: &str, files: &[(&str, &str)], dirs: &[&str]) {
        let real = root.join("devices").join(parent).join("net").join(name);
        fs::create_dir_all(&real).unwrap();
        for (file, text) in files {
            fs::write(real.join(file), format!("{text}\n")).unwrap();
        }
        for dir in dirs {
            fs::create_dir_all(real.join(dir)).unwrap();
        }
        let class = root.join("class/net");
        fs::create_dir_all(&class).unwrap();
        symlink(&real, class.join(name)).unwrap();
    }

    #[test]
    fn only_physical_ethernet_devices_are_wired() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let up = [("type", "1"), ("operstate", "up"), ("speed", "1000")];
        device(root, "platform/2490000.ethernet", "eth0", &up, &[]);
        device(
            root,
            "pci0000:00/0000:03:00.0",
            "enp3s0",
            &[
                ("type", "1"),
                ("operstate", "unknown"),
                ("carrier", "1"),
                ("speed", "-1"),
            ],
            &[],
        );
        device(
            root,
            "platform/usb",
            "eth1",
            &[("type", "1"), ("operstate", "down"), ("speed", "100")],
            &[],
        );
        device(root, "platform/wifi", "wlan0", &up, &["wireless"]);
        device(root, "platform/wifi2", "wlan1", &up, &["phy80211"]);
        device(root, "virtual", "docker0", &up, &[]);
        device(root, "virtual", "l4tbr0", &up, &[]);
        device(root, "virtual", "lo", &[("type", "772")], &[]);
        device(root, "platform/can", "can0", &[("type", "280")], &[]);

        let wired = SysfsWired::new(root.join("class/net"), addresses).read();
        assert_eq!(
            wired,
            vec![
                WiredInterface {
                    interface: "enp3s0".into(),
                    link: LinkState::Up,
                    address: None,
                    speed_mbps: None,
                },
                WiredInterface {
                    interface: "eth0".into(),
                    link: LinkState::Up,
                    address: Some(Ipv4Addr::new(192, 168, 1, 20)),
                    speed_mbps: Some(1000),
                },
                WiredInterface {
                    interface: "eth1".into(),
                    link: LinkState::Down,
                    address: None,
                    speed_mbps: None,
                },
            ],
            "a link-local address and a down link's stale speed are not shown"
        );
    }

    #[test]
    fn an_unreadable_host_shows_interfaces_without_addresses_or_nothing() {
        let root = tempfile::tempdir().unwrap();
        device(
            root.path(),
            "platform/eth",
            "eth0",
            &[("type", "1"), ("operstate", "up")],
            &[],
        );
        let wired = SysfsWired::new(root.path().join("class/net"), unreadable).read();
        assert_eq!(wired.len(), 1);
        assert_eq!(wired[0].address, None);
        assert_eq!(wired[0].speed_mbps, None, "no speed file");

        let missing = SysfsWired::new(root.path().join("nowhere"), addresses);
        assert!(missing.read().is_empty());
    }
}
