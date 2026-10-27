//! The pond's wired links as `GET /connections` shows them. Read-only: there is nothing to choose
//! on a cable (`pond-connections.md`, "Decisions").

use std::net::Ipv4Addr;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    Up,
    Down,
}

/// One wired interface, in the contract's wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WiredInterface {
    pub interface: String,
    pub link: LinkState,
    /// The first IPv4 address that is neither loopback nor link-local.
    pub address: Option<Ipv4Addr>,
    /// `None` when the OS does not say, as it does not for a link that is down.
    pub speed_mbps: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interface_uses_the_contracts_wire_names_and_nulls() {
        let up = WiredInterface {
            interface: "eth0".into(),
            link: LinkState::Up,
            address: Some(Ipv4Addr::new(192, 168, 1, 20)),
            speed_mbps: Some(1000),
        };
        assert_eq!(
            serde_json::to_value(&up).unwrap(),
            serde_json::json!({"interface": "eth0", "link": "up", "address": "192.168.1.20",
                               "speed_mbps": 1000})
        );
        let down = WiredInterface {
            interface: "eth1".into(),
            link: LinkState::Down,
            address: None,
            speed_mbps: None,
        };
        assert_eq!(
            serde_json::to_value(&down).unwrap(),
            serde_json::json!({"interface": "eth1", "link": "down", "address": null,
                               "speed_mbps": null})
        );
    }
}
