use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;

use crate::mcp::domain::marketplace::MarketplaceExtension;
use crate::mcp::ports::extension_marketplace::ExtensionMarketplace;

/// Top-level registry structure matching the JSON schema.
#[derive(serde::Deserialize)]
struct MarketplaceRegistry {
    extensions: Vec<MarketplaceExtension>,
}

/// Embedded JSON registry of curated MCP extensions.
const REGISTRY_JSON: &str = include_str!("../../extensions/marketplace_registry.json");

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

    /// Same as [`Self::new`], but rewrites repo-relative script arguments into
    /// absolute paths anchored at `root`.
    ///
    /// Some registry entries launch a script that ships with GIAP rather than a
    /// published npm package — `["-y", "tsx", "extensions/music/src/server.ts"]`.
    /// A relative arg like that is resolved by the MCP child against *its* cwd,
    /// which is inherited from whatever directory pond-server was launched in.
    /// Launch the server from anywhere but the repo root and the child dies with
    /// `ERR_MODULE_NOT_FOUND` before it can complete the MCP handshake.
    ///
    /// Anchoring the path here — once, at construction — fixes every consumer of
    /// the marketplace at the same time: the install handler, the post-OAuth
    /// restart, and the persisted config that startup auto-connect replays.
    ///
    /// `root` is the directory that *contains* `extensions/`. Discovering it is
    /// the caller's job; this crate stays free of filesystem probing.
    pub fn with_asset_root(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        let mut this = Self::new();
        for ext in &mut this.extensions {
            anchor_asset_args(&mut ext.args, root);
        }
        this
    }
}

/// Rewrites every `extensions/…` arg in `args` to an absolute path under `root`.
///
/// Exposed because configs persisted by an earlier install carry the original
/// relative args; startup auto-connect re-anchors them rather than requiring the
/// user to remove and reinstall the extension. Returns `true` if anything changed.
pub fn anchor_asset_args(args: &mut [String], root: impl AsRef<Path>) -> bool {
    let root = root.as_ref();
    let mut changed = false;
    for arg in args.iter_mut() {
        if let Some(abs) = anchor_asset_arg(arg, root) {
            *arg = abs;
            changed = true;
        }
    }
    changed
}

/// Returns the absolute form of `arg` when it names a path under the bundled
/// `extensions/` directory, or `None` when it should be passed through untouched.
///
/// Deliberately narrow: flags (`-y`), bare package names (`tsx`), scoped npm
/// packages (`@modelcontextprotocol/server-filesystem`) and absolute paths (`/`)
/// are all left exactly as the registry wrote them.
fn anchor_asset_arg(arg: &str, root: &Path) -> Option<String> {
    if arg.starts_with('-') {
        return None;
    }
    let path = PathBuf::from(arg);
    if path.is_absolute() {
        return None;
    }
    let first = path.components().next()?;
    if first.as_os_str() != "extensions" {
        return None;
    }
    Some(root.join(path).to_string_lossy().into_owned())
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

    #[tokio::test]
    async fn with_asset_root_makes_extension_paths_absolute() {
        let mp = BundledMarketplace::with_asset_root("/opt/giap");
        let music = mp.get_by_id("music").await.unwrap().expect("music entry");
        assert!(
            music
                .args
                .iter()
                .any(|a| a == "/opt/giap/extensions/music/src/server.ts"),
            "music args should be anchored at the asset root, got {:?}",
            music.args
        );
    }

    #[tokio::test]
    async fn with_asset_root_leaves_package_args_untouched() {
        let mp = BundledMarketplace::with_asset_root("/opt/giap");
        let fs = mp
            .get_by_id("filesystem")
            .await
            .unwrap()
            .expect("filesystem entry");
        assert_eq!(
            fs.args,
            BundledMarketplace::new()
                .get_by_id("filesystem")
                .await
                .unwrap()
                .unwrap()
                .args,
            "package-based entries must not be rewritten"
        );

        let music = mp.get_by_id("music").await.unwrap().expect("music entry");
        assert!(
            music.args.contains(&"-y".to_string()) && music.args.contains(&"tsx".to_string()),
            "flags and bare package names must survive rewriting, got {:?}",
            music.args
        );
    }

    #[test]
    fn no_stdio_entry_keeps_a_relative_extensions_path_after_rewrite() {
        let mp = BundledMarketplace::with_asset_root("/opt/giap");
        for ext in mp.extensions.iter().filter(|e| e.kind == "stdio") {
            for arg in &ext.args {
                assert!(
                    !arg.starts_with("extensions/"),
                    "'{}' still has a cwd-relative arg '{}' — the MCP child would \
                     resolve it against pond-server's launch directory",
                    ext.id,
                    arg
                );
            }
        }
    }

    #[test]
    fn anchor_asset_args_reports_whether_it_changed_anything() {
        let mut persisted = vec![
            "-y".to_string(),
            "tsx".to_string(),
            "extensions/music/src/server.ts".to_string(),
        ];
        assert!(anchor_asset_args(&mut persisted, "/opt/giap"));
        assert_eq!(
            persisted,
            vec!["-y", "tsx", "/opt/giap/extensions/music/src/server.ts"]
        );
        // Already anchored — a second pass is a no-op, so startup does not
        // rewrite the persisted row on every restart.
        assert!(!anchor_asset_args(&mut persisted, "/opt/giap"));

        let mut package_args = vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-filesystem".to_string(),
            "/".to_string(),
        ];
        assert!(!anchor_asset_args(&mut package_args, "/opt/giap"));
    }

    #[test]
    fn anchor_asset_arg_passes_through_non_asset_args() {
        let root = std::path::Path::new("/opt/giap");
        assert_eq!(anchor_asset_arg("-y", root), None);
        assert_eq!(anchor_asset_arg("tsx", root), None);
        assert_eq!(
            anchor_asset_arg("@modelcontextprotocol/server-filesystem", root),
            None
        );
        assert_eq!(anchor_asset_arg("/", root), None);
        assert_eq!(
            anchor_asset_arg("extensions/music/src/server.ts", root),
            Some("/opt/giap/extensions/music/src/server.ts".to_string())
        );
    }
}
