//! The pond's Wi-Fi station for each OS, behind `pond_core`'s `WifiStation` port.

pub mod unsupported;

#[cfg(any(test, feature = "test-mocks"))]
pub use pond_core::network::mocks::mock_wifi_station::MockWifiStation;
