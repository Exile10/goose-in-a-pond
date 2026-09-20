//! Durable removal of remote networking permission for an authenticated device.
use anyhow::Result;
use async_trait::async_trait;

/// Queue network revocation before the caller discards its Pond credentials.
/// Implementations must persist the intent locally even when coordination is offline.
#[async_trait]
pub trait RemoteRevocation: Send + Sync {
    /// Record an authenticated device revocation; repeated calls are idempotent.
    async fn queue(&self, device_id: &str) -> Result<()>;
}
