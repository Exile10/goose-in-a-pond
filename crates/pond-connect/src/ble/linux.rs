use std::sync::Arc;

use super::peripheral::BlePeripheral;
use super::unsupported::UnsupportedPeripheral;

pub fn platform() -> Arc<dyn BlePeripheral> {
    Arc::new(UnsupportedPeripheral::new(
        "the linux BLE adapter is not built yet",
    ))
}
