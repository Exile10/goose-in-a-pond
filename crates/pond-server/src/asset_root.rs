//! Locates the directory that holds GIAP's bundled `extensions/` tree.
//!
//! Most marketplace entries launch a published npm package, which npx resolves
//! without caring where the process was started. A few launch a script that
//! ships with GIAP instead, and those are written in the registry as paths
//! relative to `extensions/`. The MCP child resolves such a path against its own
//! cwd, inherited from pond-server's launch directory — so `cargo run` from
//! `pond-desktop/` used to kill the music extension with `ERR_MODULE_NOT_FOUND`
//! before it could finish the MCP handshake.
//!
//! [`resolve`] finds the anchor once at startup; `BundledMarketplace::with_asset_root`
//! then bakes it into the registry so no consumer has to think about cwd again.

use std::path::{Path, PathBuf};

/// Environment override naming the directory that contains `extensions/`.
///
/// The escape hatch for layouts this module cannot infer — packaged installs,
/// Jetson deployments, or a developer running the binary from an odd place.
pub const ASSET_ROOT_ENV: &str = "GIAP_ASSET_ROOT";

/// Resolves the directory containing the bundled `extensions/` tree.
///
/// Candidates are tried in order and the first one that actually holds an
/// `extensions/` directory wins:
///
/// 1. `$GIAP_ASSET_ROOT`
/// 2. the repo root baked in at compile time (dev builds only — this path does
///    not exist on a machine that did not build the binary)
/// 3. the executable's own directory, then `../Resources` beside it (bundles)
/// 4. the current working directory
///
/// Falls back to the current working directory when nothing matches, which
/// preserves the historical behaviour rather than failing startup over it.
pub fn resolve() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();

    for candidate in candidates() {
        if has_extensions_dir(&candidate) {
            // Canonicalize so the anchored path that ends up in logs, in the
            // extension card, and in the persisted mcp_servers row is the plain
            // repo path rather than something like `crates/pond-server/../..`.
            return candidate.canonicalize().unwrap_or(candidate);
        }
    }

    tracing::warn!(
        cwd = %cwd.display(),
        "no bundled extensions/ directory found; falling back to the current \
         working directory. Extensions that launch a bundled script will fail \
         to start — set {ASSET_ROOT_ENV} to the directory containing extensions/",
    );
    cwd
}

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Some(env_root) = std::env::var_os(ASSET_ROOT_ENV) {
        out.push(PathBuf::from(env_root));
    }

    // Compile-time repo root: `crates/pond-server/../..`. Present on the machine
    // that built the binary, absent in a shipped bundle.
    out.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            out.push(exe_dir.to_path_buf());
            // macOS app bundle: Contents/MacOS/pond-server -> Contents/Resources
            out.push(exe_dir.join("../Resources"));
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd);
    }

    out
}

fn has_extensions_dir(root: &Path) -> bool {
    root.join("extensions").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_extensions_dir_detects_the_bundled_tree() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert!(
            has_extensions_dir(&repo_root),
            "the repo root should contain extensions/"
        );
        assert!(!has_extensions_dir(Path::new("/nonexistent-giap-root")));
    }

    #[test]
    fn resolve_prefers_the_environment_override() {
        // `candidates()` reads process-global env, so assert on the ordering
        // contract rather than mutating it under a parallel test runner.
        let first = candidates().into_iter().next().expect("a candidate");
        match std::env::var_os(ASSET_ROOT_ENV) {
            Some(env_root) => assert_eq!(first, PathBuf::from(env_root)),
            None => assert_eq!(
                first,
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
            ),
        }
    }

    #[test]
    fn resolve_finds_a_root_holding_the_extensions_tree() {
        let root = resolve();
        assert!(
            has_extensions_dir(&root),
            "resolve() returned {} which has no extensions/ dir",
            root.display()
        );
    }

    #[test]
    fn resolve_returns_a_canonical_path() {
        let root = resolve();
        assert!(
            !root.components().any(|c| c.as_os_str() == ".."),
            "resolve() returned an uncanonicalized path: {}",
            root.display()
        );
    }
}
