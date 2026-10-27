use async_trait::async_trait;

use crate::network::domain::wired::WiredInterface;

/// The host's wired network interfaces, read live. Read-only by design.
#[async_trait]
pub trait WiredInterfaces: Send + Sync {
    /// Every wired interface, up or down, sorted by name; empty when there is none or the OS
    /// cannot be read, which the adapter logs.
    async fn wired_interfaces(&self) -> Vec<WiredInterface>;
}
