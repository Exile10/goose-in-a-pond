//! What every OS's peripheral offers the server. It only moves bytes: the GATT service carries
//! [`SetupChannel`] sessions, and the advertisement carries nothing but the card's id.

use std::sync::Arc;

use async_trait::async_trait;

use pond_core::network::domain::discriminator::Discriminator;

use crate::setup_channel::SetupChannel;

pub use pond_core::network::domain::peripheral::PeripheralStatus;

/// Exactly what a pond may advertise (spec: "Advertising"): its card's id, in the service UUID
/// and in the local name, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advertisement {
    pub pond_id: Discriminator,
}

impl Advertisement {
    /// In the advertising PDU, where every scan sees it.
    pub const fn service_uuid(self) -> u128 {
        super::advertised_service_uuid(self.pond_id)
    }

    /// In the scan response, when the stack sends one; a phone never depends on it.
    pub fn local_name(self) -> String {
        super::local_name(self.pond_id)
    }
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
