//! The setup channel's GATT layout and advertisement (`pond-setup-channel.md`, "Advertising" and
//! "GATT"), and the peripheral for each OS behind one trait, so the server wires it without `cfg`.

use std::sync::Arc;

use pond_core::network::domain::discriminator::Discriminator;

pub mod helper;
pub mod peripheral;
pub mod unsupported;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

pub use peripheral::{Advertisement, BleError, BlePeripheral, PeripheralStatus};

/// This host's peripheral: BlueZ on Linux, the CoreBluetooth helper on macOS, refusal elsewhere.
pub fn platform() -> Arc<dyn BlePeripheral> {
    #[cfg(target_os = "linux")]
    return linux::platform();
    #[cfg(target_os = "macos")]
    return macos::platform();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Arc::new(unsupported::UnsupportedPeripheral::new(
        "this OS has no BLE peripheral adapter yet",
    ));
}

/// The GATT service a phone connects to. A pond advertises [`advertised_service_uuid`] instead.
pub const SERVICE_UUID: u128 = 0x185b51bd_e038_4b6c_99b0_4e2379dd362c;
pub const SERVICE_UUID_STR: &str = "185b51bd-e038-4b6c-99b0-4e2379dd362c";

/// The service UUID's first 14 bytes; the advertised UUID ends in the card's id instead.
pub const ADVERTISED_UUID_PREFIX: u128 = SERVICE_UUID & !0xffff;
pub const ADVERTISED_UUID_PREFIX_STR: &str = "185b51bd-e038-4b6c-99b0-4e2379dd";

/// `rx`: the phone writes requests here, with response.
pub const RX_CHARACTERISTIC_UUID: u128 = 0x72172de5_d65e_47ad_857d_1b9dd7623d33;
pub const RX_CHARACTERISTIC_UUID_STR: &str = "72172de5-d65e-47ad-857d-1b9dd7623d33";

/// `tx`: the pond notifies its answers here once the phone subscribes.
pub const TX_CHARACTERISTIC_UUID: u128 = 0x1065b140_d075_431c_b981_7e1a735960cb;
pub const TX_CHARACTERISTIC_UUID_STR: &str = "1065b140-d075-431c-b981-7e1a735960cb";

pub const LOCAL_NAME_PREFIX: &str = "Pond ";

/// `185b51bd-e038-4b6c-99b0-4e2379ddc1cd` for `Pond c1cd`. The id rides in the advertising PDU
/// itself because scan responses are optional: the Jetson's BlueZ sends an empty one, and iOS
/// background scans never ask.
pub const fn advertised_service_uuid(discriminator: Discriminator) -> u128 {
    ADVERTISED_UUID_PREFIX | discriminator.value() as u128
}

/// The card id a phone reads from an advertised UUID; `None` for anyone else's UUID.
pub fn discriminator_from_service_uuid(uuid: u128) -> Option<Discriminator> {
    (uuid & !0xffff == ADVERTISED_UUID_PREFIX).then(|| Discriminator::new(uuid as u16))
}

/// `Pond c1cd`, matching the card's title. A label only: it fits nowhere but the scan response,
/// which a phone may never see.
pub fn local_name(discriminator: Discriminator) -> String {
    format!("{LOCAL_NAME_PREFIX}{discriminator}")
}

/// The discriminator a phone reads back from an advertiser's name, to find the pond on its card.
pub fn discriminator_from_local_name(name: &str) -> Option<Discriminator> {
    Discriminator::parse(name.strip_prefix(LOCAL_NAME_PREFIX)?).ok()
}

/// The canonical 8-4-4-4-12 spelling, for stacks that take UUIDs as text.
pub fn uuid_string(uuid: u128) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        uuid >> 96,
        (uuid >> 80) & 0xffff,
        (uuid >> 64) & 0xffff,
        (uuid >> 48) & 0xffff,
        uuid & 0xffff_ffff_ffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uuids_are_the_specs_in_both_spellings() {
        assert_eq!(uuid_string(SERVICE_UUID), SERVICE_UUID_STR);
        assert_eq!(
            uuid_string(RX_CHARACTERISTIC_UUID),
            RX_CHARACTERISTIC_UUID_STR
        );
        assert_eq!(
            uuid_string(TX_CHARACTERISTIC_UUID),
            TX_CHARACTERISTIC_UUID_STR
        );
    }

    #[test]
    fn the_advertised_uuid_ends_in_the_cards_id() {
        let id = Discriminator::new(0x7f3a);
        assert_eq!(
            uuid_string(advertised_service_uuid(id)),
            "185b51bd-e038-4b6c-99b0-4e2379dd7f3a"
        );
        assert!(uuid_string(ADVERTISED_UUID_PREFIX).starts_with(ADVERTISED_UUID_PREFIX_STR));
        assert_eq!(
            discriminator_from_service_uuid(advertised_service_uuid(id)),
            Some(id)
        );
        assert_eq!(
            discriminator_from_service_uuid(SERVICE_UUID),
            Some(Discriminator::new(0x362c))
        );
        assert_eq!(
            discriminator_from_service_uuid(RX_CHARACTERISTIC_UUID),
            None
        );
        assert_eq!(
            discriminator_from_service_uuid(advertised_service_uuid(id) ^ (1 << 16)),
            None
        );
    }

    #[test]
    fn the_local_name_carries_the_cards_id_in_lowercase() {
        let id = Discriminator::new(0xc1cd);
        assert_eq!(local_name(id), "Pond c1cd");
        assert_eq!(discriminator_from_local_name("Pond c1cd"), Some(id));
        assert_eq!(discriminator_from_local_name("Pond C1CD"), Some(id));
        assert_eq!(discriminator_from_local_name("Speaker c1cd"), None);
        assert_eq!(discriminator_from_local_name("Pond c1c"), None);
    }
}
