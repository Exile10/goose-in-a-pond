//! The setup channel's radios as the household's status view reads them.

use std::sync::Arc;

use async_trait::async_trait;
use pond_core::network::domain::peripheral::PeripheralStatus;
use pond_core::network::domain::wifi::WifiCapabilities;
use pond_core::network::ports::setup_radios::SetupRadios;
use pond_core::network::ports::wifi_station::WifiStation;

use crate::ble::BlePeripheral;
use crate::setup_channel::SetupChannel;

pub struct SetupRadioStatus {
    peripheral: Arc<dyn BlePeripheral>,
    wifi: Arc<dyn WifiStation>,
    channel: SetupChannel,
}

impl SetupRadioStatus {
    pub fn new(
        peripheral: Arc<dyn BlePeripheral>,
        wifi: Arc<dyn WifiStation>,
        channel: SetupChannel,
    ) -> Self {
        Self {
            peripheral,
            wifi,
            channel,
        }
    }
}

#[async_trait]
impl SetupRadios for SetupRadioStatus {
    fn ble(&self) -> PeripheralStatus {
        self.peripheral.status()
    }

    fn session_open(&self) -> bool {
        self.channel.is_session_open()
    }

    fn advertising_as(&self) -> Option<String> {
        (self.peripheral.status() == PeripheralStatus::Advertising)
            .then(|| crate::ble::local_name(self.channel.config().discriminator))
    }

    async fn wifi(&self) -> WifiCapabilities {
        self.wifi.capabilities().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pond_core::network::mocks::mock_wifi_station::MockWifiStation;

    use super::*;
    use crate::ble::{Advertisement, BleError};
    use crate::test_support::test_pond;

    struct Fixed(Mutex<PeripheralStatus>);

    #[async_trait]
    impl BlePeripheral for Fixed {
        async fn start(&self, _: Arc<SetupChannel>) -> Result<(), BleError> {
            Ok(())
        }
        async fn advertise(&self, _: Option<Advertisement>) -> Result<(), BleError> {
            Ok(())
        }
        async fn shutdown(&self) {}
        fn status(&self) -> PeripheralStatus {
            self.0.lock().unwrap().clone()
        }
    }

    #[tokio::test]
    async fn the_label_is_named_only_while_advertising() {
        let peripheral = Arc::new(Fixed(Mutex::new(PeripheralStatus::Serving)));
        let radios = SetupRadioStatus::new(
            peripheral.clone(),
            Arc::new(MockWifiStation::new()),
            test_pond().channel,
        );
        assert_eq!(radios.advertising_as(), None);
        *peripheral.0.lock().unwrap() = PeripheralStatus::Advertising;
        assert_eq!(radios.advertising_as().as_deref(), Some("Pond 7f3a"));
        *peripheral.0.lock().unwrap() = PeripheralStatus::unavailable("Bluetooth is off");
        assert_eq!(radios.advertising_as(), None);
        assert!(radios.wifi().await.forget.is_available());
    }
}
