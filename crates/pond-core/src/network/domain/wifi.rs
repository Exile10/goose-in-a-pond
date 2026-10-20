//! What the pond's Wi-Fi station sees and does, in the setup channel's wire shapes
//! (`docs/architecture/pond-setup-channel.md`, "After the exchange").

use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::network::domain::capability::Availability;
use crate::shared::domain::wipe::wipe_string;

/// The strongest signal a network reports, as a percentage.
pub const MAX_SIGNAL: u8 = 100;
/// 802.11 caps an SSID at 32 octets.
pub const MAX_SSID_BYTES: usize = 32;
/// WPA allows 63 characters, or 64 hex digits for a raw key.
pub const MAX_PASSPHRASE_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiSecurity {
    Open,
    Wep,
    Wpa,
    Wpa2,
    Wpa3,
    Enterprise,
}

/// One network the station can see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub security: WifiSecurity,
    /// 0 to [`MAX_SIGNAL`].
    pub signal: u8,
    /// The OS already holds credentials for it.
    pub known: bool,
}

/// An SSID the station can be asked to join.
pub fn is_valid_ssid(ssid: &str) -> bool {
    !ssid.is_empty() && ssid.len() <= MAX_SSID_BYTES
}

/// Whether a passphrase is all a join needs. The pond can only send a passphrase, and an
/// enterprise network asks for a username and a certificate, so it is refused before any attempt.
pub fn joinable(security: WifiSecurity) -> Result<(), WifiJoinError> {
    match security {
        WifiSecurity::Enterprise => Err(WifiJoinError::UnsupportedSecurity),
        WifiSecurity::Open
        | WifiSecurity::Wep
        | WifiSecurity::Wpa
        | WifiSecurity::Wpa2
        | WifiSecurity::Wpa3 => Ok(()),
    }
}

/// One entry per SSID, strongest first, as the phone shows them. Every access point of a network
/// reports separately, and a hidden one reports no name at all.
pub fn rank_networks(networks: Vec<WifiNetwork>) -> Vec<WifiNetwork> {
    let mut by_ssid: HashMap<String, WifiNetwork> = HashMap::with_capacity(networks.len());
    for mut network in networks {
        if !is_valid_ssid(&network.ssid) {
            continue;
        }
        network.signal = network.signal.min(MAX_SIGNAL);
        match by_ssid.get_mut(&network.ssid) {
            Some(kept) => {
                let known = kept.known || network.known;
                if network.signal > kept.signal {
                    *kept = network;
                }
                kept.known = known;
            }
            None => {
                by_ssid.insert(network.ssid.clone(), network);
            }
        }
    }
    let mut ranked: Vec<WifiNetwork> = by_ssid.into_values().collect();
    ranked.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.ssid.cmp(&b.ssid)));
    ranked
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a Wi-Fi passphrase is 1 to {MAX_PASSPHRASE_BYTES} bytes")]
pub struct InvalidPassphrase;

/// On its way to the OS's own credential store. No `Display`, `Serialize` or `Clone` and a
/// redacting `Debug`, so it is never logged, returned or copied; wiped on drop.
pub struct WifiPassphrase(String);

impl WifiPassphrase {
    pub fn new(mut text: String) -> Result<Self, InvalidPassphrase> {
        if text.is_empty() || text.len() > MAX_PASSPHRASE_BYTES {
            wipe_string(&mut text);
            return Err(InvalidPassphrase);
        }
        Ok(Self(text))
    }

    /// For the adapter that hands it to the OS, and for nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Drop for WifiPassphrase {
    fn drop(&mut self) {
        wipe_string(&mut self.0);
    }
}

impl fmt::Debug for WifiPassphrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WifiPassphrase(redacted)")
    }
}

impl<'de> Deserialize<'de> for WifiPassphrase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The error names the rule, never the input.
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// The network a join ended on, with the address the phone carries on to over HTTPS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinedNetwork {
    pub ssid: String,
    pub ipv4: Ipv4Addr,
}

/// Why a join failed. Each maps to one wire code the phone branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WifiJoinError {
    #[error("the network refused the passphrase")]
    WrongPassword,
    #[error("no network with that name is in range")]
    NotFound,
    #[error("joined, but no IPv4 address was assigned")]
    NoAddress,
    #[error("the network did not answer in time")]
    Timeout,
    #[error("this pond is not allowed to change its Wi-Fi")]
    NotPermitted,
    #[error("this host cannot join a Wi-Fi network")]
    Unsupported,
    #[error("the Wi-Fi station is busy")]
    Busy,
    #[error("this network asks for more than a passphrase")]
    UnsupportedSecurity,
}

impl WifiJoinError {
    pub fn code(self) -> &'static str {
        match self {
            Self::WrongPassword => "wrong_password",
            Self::NotFound => "not_found",
            Self::NoAddress => "no_address",
            Self::Timeout => "timeout",
            Self::NotPermitted => "not_permitted",
            Self::Unsupported => "unsupported",
            Self::Busy => "busy",
            Self::UnsupportedSecurity => "unsupported_security",
        }
    }
}

/// Why a scan failed, in the join's codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WifiScanError {
    #[error("this pond is not allowed to scan for Wi-Fi")]
    NotPermitted,
    #[error("this host cannot scan for Wi-Fi")]
    Unsupported,
    #[error("the Wi-Fi station is busy")]
    Busy,
    /// Only the HTTPS route's budget produces this; the station itself always answers.
    #[error("the scan did not finish in time")]
    Timeout,
}

impl WifiScanError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotPermitted => "not_permitted",
            Self::Unsupported => "unsupported",
            Self::Busy => "busy",
            Self::Timeout => "timeout",
        }
    }
}

/// Why a saved network was not forgotten, in the join's codes where the meaning is the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WifiForgetError {
    #[error("no saved network has that name")]
    NotFound,
    #[error("the pond is on that network now")]
    InUse,
    #[error("this pond is not allowed to forget a saved network")]
    NotPermitted,
    #[error("this host cannot forget a Wi-Fi network")]
    Unsupported,
    #[error("the Wi-Fi station is busy")]
    Busy,
}

impl WifiForgetError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::InUse => "in_use",
            Self::NotPermitted => "not_permitted",
            Self::Unsupported => "unsupported",
            Self::Busy => "busy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkState {
    Connected,
    Connecting,
    Disconnected,
}

/// The station's link as `status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub state: NetworkState,
    pub ssid: Option<String>,
    pub ipv4: Option<Ipv4Addr>,
}

impl NetworkStatus {
    pub fn disconnected() -> Self {
        Self {
            state: NetworkState::Disconnected,
            ssid: None,
            ipv4: None,
        }
    }
}

/// The network the station is on now, as the saved network it was joined with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentNetwork {
    pub ssid: String,
    /// 0 to [`MAX_SIGNAL`]; `None` when the OS does not say.
    pub signal: Option<u8>,
}

/// One entry of the HTTPS networks list: the setup channel's [`WifiNetwork`] plus `current`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListedNetwork {
    pub ssid: String,
    pub security: WifiSecurity,
    pub signal: u8,
    pub known: bool,
    pub current: bool,
}

impl ListedNetwork {
    pub fn new(network: WifiNetwork, current: Option<&str>) -> Self {
        Self {
            current: current == Some(network.ssid.as_str()),
            ssid: network.ssid,
            security: network.security,
            signal: network.signal.min(MAX_SIGNAL),
            known: network.known,
        }
    }
}

/// What a station can do on this host now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiCapabilities {
    pub scan: Availability,
    pub join: Availability,
    pub forget: Availability,
}

impl WifiCapabilities {
    pub fn available() -> Self {
        Self {
            scan: Availability::Available,
            join: Availability::Available,
            forget: Availability::Available,
        }
    }

    pub fn unsupported(reason: &str) -> Self {
        Self {
            scan: Availability::unavailable(reason),
            join: Availability::unavailable(reason),
            forget: Availability::unavailable(reason),
        }
    }

    /// No adapter, no station on this OS, or the radio switched off. Forget is left out: a Mac
    /// never offers it, and that says nothing about whether it has a station.
    pub fn is_unavailable(&self) -> bool {
        !self.scan.is_available() && !self.join.is_available()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiLinkState {
    Connected,
    Disconnected,
    Unavailable,
}

/// The Wi-Fi section of `GET /connections`: one state, and the details that state has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiLink {
    pub state: WifiLinkState,
    /// Why, for `unavailable` only.
    pub reason: Option<String>,
    pub ssid: Option<String>,
    pub address: Option<Ipv4Addr>,
    pub signal: Option<u8>,
}

impl WifiLink {
    /// A link still joining reads as disconnected: the household cannot reach the pond on it yet.
    /// A connected link wins over the capabilities, which may fail to read on their own.
    pub fn read(
        status: &NetworkStatus,
        current: Option<&CurrentNetwork>,
        capabilities: &WifiCapabilities,
    ) -> Self {
        if status.state == NetworkState::Connected {
            let ssid = current
                .map(|current| current.ssid.clone())
                .or_else(|| status.ssid.clone());
            let signal = current
                .filter(|current| Some(&current.ssid) == ssid.as_ref())
                .and_then(|current| current.signal)
                .map(|signal| signal.min(MAX_SIGNAL));
            return Self {
                state: WifiLinkState::Connected,
                reason: None,
                ssid,
                address: status.ipv4,
                signal,
            };
        }
        let (state, reason) = if capabilities.is_unavailable() {
            let reason = capabilities
                .join
                .reason()
                .or(capabilities.scan.reason())
                .map(str::to_string);
            (WifiLinkState::Unavailable, reason)
        } else {
            (WifiLinkState::Disconnected, None)
        };
        Self {
            state,
            reason,
            ssid: None,
            address: None,
            signal: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(ssid: &str, signal: u8, known: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            security: WifiSecurity::Wpa2,
            signal,
            known,
        }
    }

    #[test]
    fn ranking_keeps_the_strongest_entry_per_ssid_and_orders_strongest_first() {
        let ranked = rank_networks(vec![
            network("Kitchen", 40, false),
            network("Home", 30, true),
            network("Home", 80, false),
            network("Attic", 80, false),
            network("", 99, false),
            network(&"x".repeat(MAX_SSID_BYTES + 1), 99, false),
            network("Loud", 250, false),
        ]);
        let order: Vec<(&str, u8, bool)> = ranked
            .iter()
            .map(|n| (n.ssid.as_str(), n.signal, n.known))
            .collect();
        assert_eq!(
            order,
            vec![
                ("Loud", MAX_SIGNAL, false),
                ("Attic", 80, false),
                ("Home", 80, true),
                ("Kitchen", 40, false),
            ],
            "hidden and oversized names drop, ties order by name, a known entry stays known"
        );
    }

    #[test]
    fn security_and_status_use_the_wire_names() {
        assert_eq!(
            serde_json::to_value([
                WifiSecurity::Open,
                WifiSecurity::Wep,
                WifiSecurity::Wpa,
                WifiSecurity::Wpa2,
                WifiSecurity::Wpa3,
                WifiSecurity::Enterprise
            ])
            .unwrap(),
            serde_json::json!(["open", "wep", "wpa", "wpa2", "wpa3", "enterprise"])
        );
        let status = NetworkStatus {
            state: NetworkState::Connected,
            ssid: Some("Home".into()),
            ipv4: Some(Ipv4Addr::new(192, 168, 1, 42)),
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({"state": "connected", "ssid": "Home", "ipv4": "192.168.1.42"})
        );
    }

    #[test]
    fn error_codes_are_the_specs() {
        let join: Vec<&str> = [
            WifiJoinError::WrongPassword,
            WifiJoinError::NotFound,
            WifiJoinError::NoAddress,
            WifiJoinError::Timeout,
            WifiJoinError::NotPermitted,
            WifiJoinError::Unsupported,
            WifiJoinError::Busy,
            WifiJoinError::UnsupportedSecurity,
        ]
        .into_iter()
        .map(WifiJoinError::code)
        .collect();
        assert_eq!(
            join,
            [
                "wrong_password",
                "not_found",
                "no_address",
                "timeout",
                "not_permitted",
                "unsupported",
                "busy",
                "unsupported_security"
            ]
        );
        assert_eq!(WifiScanError::NotPermitted.code(), "not_permitted");
        assert_eq!(WifiScanError::Unsupported.code(), "unsupported");
        assert_eq!(WifiScanError::Busy.code(), "busy");
        assert_eq!(WifiScanError::Timeout.code(), "timeout");
        let forget: Vec<&str> = [
            WifiForgetError::NotFound,
            WifiForgetError::InUse,
            WifiForgetError::NotPermitted,
            WifiForgetError::Unsupported,
            WifiForgetError::Busy,
        ]
        .into_iter()
        .map(WifiForgetError::code)
        .collect();
        assert_eq!(
            forget,
            [
                "not_found",
                "in_use",
                "not_permitted",
                "unsupported",
                "busy"
            ]
        );
    }

    #[test]
    fn only_an_enterprise_network_asks_for_more_than_a_passphrase() {
        assert_eq!(
            joinable(WifiSecurity::Enterprise),
            Err(WifiJoinError::UnsupportedSecurity)
        );
        for security in [
            WifiSecurity::Open,
            WifiSecurity::Wep,
            WifiSecurity::Wpa,
            WifiSecurity::Wpa2,
            WifiSecurity::Wpa3,
        ] {
            assert_eq!(joinable(security), Ok(()), "{security:?}");
        }
    }

    #[test]
    fn a_listed_network_is_current_only_by_its_exact_name() {
        let listed = ListedNetwork::new(network("Home", 70, true), Some("Home"));
        assert_eq!(
            serde_json::to_value(&listed).unwrap(),
            serde_json::json!({"ssid": "Home", "security": "wpa2", "signal": 70, "known": true,
                               "current": true})
        );
        assert!(!ListedNetwork::new(network("Home", 70, true), Some("home")).current);
        assert!(!ListedNetwork::new(network("Home", 70, true), None).current);
    }

    fn connected(ssid: Option<&str>) -> NetworkStatus {
        NetworkStatus {
            state: NetworkState::Connected,
            ssid: ssid.map(str::to_string),
            ipv4: Some(Ipv4Addr::new(192, 168, 1, 11)),
        }
    }

    #[test]
    fn a_connected_link_names_its_network_signal_and_address() {
        let current = CurrentNetwork {
            ssid: "Home".into(),
            signal: Some(70),
        };
        let link = WifiLink::read(
            &connected(Some("Home")),
            Some(&current),
            &WifiCapabilities::unsupported("permissions unreadable"),
        );
        assert_eq!(
            link,
            WifiLink {
                state: WifiLinkState::Connected,
                reason: None,
                ssid: Some("Home".into()),
                address: Some(Ipv4Addr::new(192, 168, 1, 11)),
                signal: Some(70),
            },
            "a working link is reported even when the capabilities cannot be read"
        );
        let other = CurrentNetwork {
            ssid: "Cafe".into(),
            signal: Some(30),
        };
        let link = WifiLink::read(
            &connected(Some("Home")),
            None,
            &WifiCapabilities::available(),
        );
        assert_eq!((link.ssid.as_deref(), link.signal), (Some("Home"), None));
        let link = WifiLink::read(
            &connected(None),
            Some(&other),
            &WifiCapabilities::available(),
        );
        assert_eq!(
            (link.ssid.as_deref(), link.signal),
            (Some("Cafe"), Some(30)),
            "the saved network's name stands in when the OS withholds the live one"
        );
    }

    #[test]
    fn a_link_is_unavailable_only_when_neither_scan_nor_join_works() {
        let joining = NetworkStatus {
            state: NetworkState::Connecting,
            ssid: Some("Home".into()),
            ipv4: None,
        };
        let link = WifiLink::read(&joining, None, &WifiCapabilities::available());
        assert_eq!(
            link,
            WifiLink {
                state: WifiLinkState::Disconnected,
                reason: None,
                ssid: None,
                address: None,
                signal: None,
            }
        );
        let link = WifiLink::read(
            &NetworkStatus::disconnected(),
            None,
            &WifiCapabilities::unsupported("this host has no Wi-Fi adapter"),
        );
        assert_eq!(link.state, WifiLinkState::Unavailable);
        assert_eq!(
            link.reason.as_deref(),
            Some("this host has no Wi-Fi adapter")
        );

        let mac = WifiCapabilities {
            scan: Availability::Available,
            join: Availability::Available,
            forget: Availability::unavailable("administrators only"),
        };
        assert!(!mac.is_unavailable());
        assert_eq!(
            WifiLink::read(&NetworkStatus::disconnected(), None, &mac).state,
            WifiLinkState::Disconnected
        );
    }

    #[test]
    fn a_passphrase_never_prints_and_is_bounded() {
        let passphrase = WifiPassphrase::new("correct horse battery".into()).unwrap();
        assert_eq!(passphrase.expose(), "correct horse battery");
        assert_eq!(format!("{passphrase:?}"), "WifiPassphrase(redacted)");
        assert!(WifiPassphrase::new(String::new()).is_err());
        assert!(WifiPassphrase::new("x".repeat(MAX_PASSPHRASE_BYTES + 1)).is_err());
        assert!(WifiPassphrase::new("x".repeat(MAX_PASSPHRASE_BYTES)).is_ok());
    }

    #[test]
    fn a_passphrase_deserialises_and_a_bad_one_does_not_echo_itself() {
        #[derive(Debug, Deserialize)]
        struct Join {
            passphrase: Option<WifiPassphrase>,
        }
        let join: Join = serde_json::from_str(r#"{"passphrase": "hunter2hunter2"}"#).unwrap();
        assert_eq!(join.passphrase.as_ref().unwrap().expose(), "hunter2hunter2");
        assert!(!format!("{join:?}").contains("hunter2"));

        let long = "y".repeat(MAX_PASSPHRASE_BYTES + 1);
        let error = serde_json::from_str::<Join>(&format!(r#"{{"passphrase": "{long}"}}"#))
            .unwrap_err()
            .to_string();
        assert!(!error.contains(&long), "{error}");
    }

    #[test]
    fn ssids_are_one_to_thirty_two_bytes() {
        assert!(!is_valid_ssid(""));
        assert!(is_valid_ssid("Home"));
        assert!(is_valid_ssid(&"x".repeat(MAX_SSID_BYTES)));
        assert!(!is_valid_ssid(&"x".repeat(MAX_SSID_BYTES + 1)));
    }
}
