use std::sync::Arc;

use pond_core::network::ports::wifi_station::WifiStation;

use super::unsupported::UnsupportedWifiStation;

pub fn platform() -> Arc<dyn WifiStation> {
    Arc::new(UnsupportedWifiStation::new(
        "the macos Wi-Fi adapter is not built yet",
    ))
}
