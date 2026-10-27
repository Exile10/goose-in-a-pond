use async_trait::async_trait;
use pond_core::network::domain::wifi::{
    CurrentNetwork, JoinedNetwork, NetworkStatus, WifiCapabilities, WifiForgetError, WifiJoinError,
    WifiNetwork, WifiPassphrase, WifiScanError,
};
use pond_core::network::ports::wifi_station::WifiStation;

/// The station on a host with no adapter for its OS: it says why, and refuses everything.
pub struct UnsupportedWifiStation {
    reason: String,
}

impl UnsupportedWifiStation {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl WifiStation for UnsupportedWifiStation {
    async fn capabilities(&self) -> WifiCapabilities {
        WifiCapabilities::unsupported(&self.reason)
    }

    async fn scan(&self) -> Result<Vec<WifiNetwork>, WifiScanError> {
        Err(WifiScanError::Unsupported)
    }

    async fn join(
        &self,
        _ssid: &str,
        _passphrase: Option<&WifiPassphrase>,
        _hidden: bool,
    ) -> Result<JoinedNetwork, WifiJoinError> {
        Err(WifiJoinError::Unsupported)
    }

    async fn status(&self) -> NetworkStatus {
        NetworkStatus::disconnected()
    }

    async fn current(&self) -> Option<CurrentNetwork> {
        None
    }

    async fn forget(&self, _ssid: &str) -> Result<(), WifiForgetError> {
        Err(WifiForgetError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn it_refuses_everything_and_says_why() {
        let station = UnsupportedWifiStation::new("Windows has no station adapter yet");
        let capabilities = station.capabilities().await;
        assert_eq!(
            capabilities.join.reason(),
            Some("Windows has no station adapter yet")
        );
        assert_eq!(station.scan().await, Err(WifiScanError::Unsupported));
        assert_eq!(
            station.join("Home", None, false).await,
            Err(WifiJoinError::Unsupported)
        );
        assert_eq!(station.status().await, NetworkStatus::disconnected());
        assert_eq!(station.current().await, None);
        assert_eq!(
            station.forget("Home").await,
            Err(WifiForgetError::Unsupported)
        );
        assert_eq!(
            capabilities.forget.reason(),
            Some("Windows has no station adapter yet")
        );
    }
}
