use anyhow::Result;
use async_trait::async_trait;

use crate::domain::marketplace::MarketplaceExtension;
use crate::ports::extension_marketplace::ExtensionMarketplace;

/// Top-level registry structure matching the JSON schema.
#[derive(serde::Deserialize)]
struct MarketplaceRegistry {
    extensions: Vec<MarketplaceExtension>,
}

/// Embedded JSON registry of curated MCP extensions.
const REGISTRY_JSON: &str = include_str!("../extensions/marketplace_registry.json");

/// A marketplace backed by a bundled JSON registry compiled into the binary.
///
/// Zero external dependencies — the registry is parsed once at construction
/// and served from memory. No network, no database.
pub struct BundledMarketplace {
    extensions: Vec<MarketplaceExtension>,
}

impl BundledMarketplace {
    pub fn new() -> Self {
        let registry: MarketplaceRegistry =
            serde_json::from_str(REGISTRY_JSON).expect("invalid marketplace registry JSON");
        Self {
            extensions: registry.extensions,
        }
    }
}

impl Default for BundledMarketplace {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtensionMarketplace for BundledMarketplace {
    async fn list_available(&self) -> Result<Vec<MarketplaceExtension>> {
        Ok(self.extensions.clone())
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<MarketplaceExtension>> {
        Ok(self.extensions.iter().find(|e| e.id == id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_parses_successfully() {
        let mp = BundledMarketplace::new();
        assert!(
            !mp.extensions.is_empty(),
            "registry should contain extensions"
        );
    }

    #[test]
    fn all_entries_have_required_fields() {
        let mp = BundledMarketplace::new();
        for ext in &mp.extensions {
            assert!(!ext.id.is_empty(), "id must not be empty");
            assert!(!ext.name.is_empty(), "name must not be empty");
            assert!(!ext.description.is_empty(), "description must not be empty");
            assert!(!ext.kind.is_empty(), "kind must not be empty");
            assert!(!ext.category.is_empty(), "category must not be empty");
            assert!(!ext.author.is_empty(), "author must not be empty");
        }
    }

    #[test]
    fn ids_are_unique() {
        let mp = BundledMarketplace::new();
        let mut seen = std::collections::HashSet::new();
        for ext in &mp.extensions {
            assert!(seen.insert(&ext.id), "duplicate id: {}", ext.id);
        }
    }

    #[test]
    fn stdio_extensions_have_command() {
        let mp = BundledMarketplace::new();
        for ext in mp.extensions.iter().filter(|e| e.kind == "stdio") {
            assert!(
                ext.command.is_some(),
                "stdio extension '{}' must have a command",
                ext.id
            );
        }
    }

    #[tokio::test]
    async fn list_available_returns_all() {
        let mp = BundledMarketplace::new();
        let all = mp.list_available().await.unwrap();
        assert_eq!(all.len(), mp.extensions.len());
    }

    #[tokio::test]
    async fn get_by_id_found() {
        let mp = BundledMarketplace::new();
        let ext = mp.get_by_id("filesystem").await.unwrap();
        assert!(ext.is_some());
        assert_eq!(ext.unwrap().name, "Filesystem");
    }

    #[tokio::test]
    async fn get_by_id_not_found() {
        let mp = BundledMarketplace::new();
        let ext = mp.get_by_id("nonexistent").await.unwrap();
        assert!(ext.is_none());
    }

    #[test]
    fn featured_extensions_exist() {
        let mp = BundledMarketplace::new();
        let featured: Vec<_> = mp.extensions.iter().filter(|e| e.featured).collect();
        assert!(
            !featured.is_empty(),
            "at least one extension should be featured"
        );
    }
}
