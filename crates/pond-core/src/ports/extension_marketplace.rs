use anyhow::Result;
use async_trait::async_trait;

use crate::domain::marketplace::MarketplaceExtension;

/// Read-only catalogue of curated MCP extensions available for installation.
#[async_trait]
pub trait ExtensionMarketplace: Send + Sync {
    /// List all available extensions in the marketplace.
    async fn list_available(&self) -> Result<Vec<MarketplaceExtension>>;

    /// Get a specific extension by ID.
    async fn get_by_id(&self, id: &str) -> Result<Option<MarketplaceExtension>>;
}
