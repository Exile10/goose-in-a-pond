use async_trait::async_trait;

use crate::network::domain::peripheral::PeripheralStatus;
use crate::network::domain::wifi::WifiCapabilities;

/// What the setup channel's radios can do now, for the household's status view. Reads only:
/// nothing here starts, stops or reaches a session.
#[async_trait]
pub trait SetupRadios: Send + Sync {
    fn ble(&self) -> PeripheralStatus;

    /// A phone holds the setup channel's one session.
    fn session_open(&self) -> bool;

    /// The label phones see while the pond advertises (`Pond 4e81`); `None` when it does not.
    fn advertising_as(&self) -> Option<String>;

    async fn wifi(&self) -> WifiCapabilities;
}
