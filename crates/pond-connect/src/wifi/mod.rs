//! The pond's Wi-Fi station for each OS, behind `pond_core`'s `WifiStation` port.

use std::sync::Arc;

use async_trait::async_trait;
use pond_core::network::domain::wifi::{
    CurrentNetwork, JoinedNetwork, NetworkStatus, WifiCapabilities, WifiForgetError, WifiJoinError,
    WifiNetwork, WifiPassphrase, WifiScanError,
};
use pond_core::network::ports::wifi_station::WifiStation;

pub mod unsupported;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

/// This host's station: NetworkManager on Linux, CoreWLAN on macOS, refusal elsewhere. Build it
/// once and share it: its operations exclude each other only within one instance.
pub fn platform() -> Arc<dyn WifiStation> {
    #[cfg(target_os = "linux")]
    let station = linux::platform();
    #[cfg(target_os = "macos")]
    let station = macos::platform();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let station: Arc<dyn WifiStation> = Arc::new(unsupported::UnsupportedWifiStation::new(
        "this OS has no Wi-Fi adapter yet",
    ));
    Arc::new(SerialStation::new(station))
}

/// One radio operation at a time across every transport: a setup-channel join, an HTTPS join, a
/// scan and a forget each answer `busy` while another runs. Reads pass straight through.
pub struct SerialStation {
    inner: Arc<dyn WifiStation>,
    radio: tokio::sync::Mutex<()>,
}

impl SerialStation {
    pub fn new(inner: Arc<dyn WifiStation>) -> Self {
        Self {
            inner,
            radio: tokio::sync::Mutex::new(()),
        }
    }
}

#[async_trait]
impl WifiStation for SerialStation {
    async fn capabilities(&self) -> WifiCapabilities {
        self.inner.capabilities().await
    }

    async fn scan(&self) -> Result<Vec<WifiNetwork>, WifiScanError> {
        let Ok(_radio) = self.radio.try_lock() else {
            return Err(WifiScanError::Busy);
        };
        self.inner.scan().await
    }

    async fn join(
        &self,
        ssid: &str,
        passphrase: Option<&WifiPassphrase>,
        hidden: bool,
    ) -> Result<JoinedNetwork, WifiJoinError> {
        let Ok(_radio) = self.radio.try_lock() else {
            return Err(WifiJoinError::Busy);
        };
        self.inner.join(ssid, passphrase, hidden).await
    }

    async fn status(&self) -> NetworkStatus {
        self.inner.status().await
    }

    async fn current(&self) -> Option<CurrentNetwork> {
        self.inner.current().await
    }

    async fn forget(&self, ssid: &str) -> Result<(), WifiForgetError> {
        let Ok(_radio) = self.radio.try_lock() else {
            return Err(WifiForgetError::Busy);
        };
        self.inner.forget(ssid).await
    }
}

#[cfg(any(test, feature = "test-mocks"))]
pub use pond_core::network::mocks::mock_wifi_station::MockWifiStation;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pond_core::network::domain::wifi::{NetworkState, WifiSecurity};
    use pond_core::network::mocks::mock_wifi_station::sample_network;
    use tokio::sync::Notify;

    use super::*;

    /// The mock, whose joins wait for `release` so a test can hold the radio.
    struct Held {
        inner: MockWifiStation,
        release: Notify,
    }

    #[async_trait]
    impl WifiStation for Held {
        async fn capabilities(&self) -> WifiCapabilities {
            self.inner.capabilities().await
        }
        async fn scan(&self) -> Result<Vec<WifiNetwork>, WifiScanError> {
            self.inner.scan().await
        }
        async fn join(
            &self,
            ssid: &str,
            passphrase: Option<&WifiPassphrase>,
            hidden: bool,
        ) -> Result<JoinedNetwork, WifiJoinError> {
            self.release.notified().await;
            self.inner.join(ssid, passphrase, hidden).await
        }
        async fn status(&self) -> NetworkStatus {
            self.inner.status().await
        }
        async fn current(&self) -> Option<CurrentNetwork> {
            self.inner.current().await
        }
        async fn forget(&self, ssid: &str) -> Result<(), WifiForgetError> {
            self.inner.forget(ssid).await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_radio_operation_is_busy_while_a_join_runs_and_reads_are_not() {
        let held = Arc::new(Held {
            inner: MockWifiStation::new()
                .with_network(sample_network("Home", WifiSecurity::Wpa2, 70), None)
                .with_saved("Cafe"),
            release: Notify::new(),
        });
        let station = Arc::new(SerialStation::new(held.clone()));
        let first = tokio::spawn({
            let station = station.clone();
            async move { station.join("Home", None, false).await }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        assert_eq!(
            station.join("Home", None, false).await,
            Err(WifiJoinError::Busy),
            "the setup channel and the HTTPS route share this instance"
        );
        assert_eq!(station.scan().await, Err(WifiScanError::Busy));
        assert_eq!(station.forget("Cafe").await, Err(WifiForgetError::Busy));
        assert_eq!(station.status().await.state, NetworkState::Disconnected);
        assert!(station.capabilities().await.join.is_available());

        held.release.notify_one();
        assert!(first.await.unwrap().is_ok());
        assert_eq!(station.current().await.unwrap().ssid, "Home");
        assert_eq!(station.scan().await.unwrap().len(), 1);
        station.forget("Cafe").await.unwrap();
    }
}
