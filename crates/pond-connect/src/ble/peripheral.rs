//! What every OS's peripheral offers the server. It only moves bytes: the GATT service carries
//! [`SetupChannel`] sessions, and the advertisement carries nothing but the local name.

use std::sync::Arc;

use async_trait::async_trait;

use crate::setup_channel::SetupChannel;

/// Exactly what a pond may advertise beside the service UUID (spec: "Advertising").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    /// `Pond <id>`; it travels in the scan response where the UUID leaves no room.
    pub local_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeripheralStatus {
    /// Not started yet.
    Off,
    /// The GATT service is registered and connections are served; nothing is advertised.
    Serving,
    Advertising,
    /// Says why, so the dashboard and `doctor` can tell the household (no radio, no permission).
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum BleError {
    #[error("bluetooth is unavailable here: {0}")]
    Unavailable(String),
    #[error("bluetooth failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait BlePeripheral: Send + Sync {
    /// Registers the GATT service; every connection it accepts is one `channel.open()` session.
    async fn start(&self, channel: Arc<SetupChannel>) -> Result<(), BleError>;

    /// `Some` advertises exactly this; `None` stops advertising. Neither touches an open session,
    /// which ends only when the phone disconnects or the session itself times out.
    async fn advertise(&self, advertisement: Option<Advertisement>) -> Result<(), BleError>;

    async fn shutdown(&self);

    fn status(&self) -> PeripheralStatus;
}
