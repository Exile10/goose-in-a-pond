//! Process-global handle on the pond's secret store, for MCP tools that need a
//! third-party API key.
//!
//! PAI-2 P2. These keys used to be `api_key_*` fields on `Settings`, read here
//! through the settings repository the server was handed at registration time.
//! `GET /api/v1/settings` serialises that struct wholesale, so every key was in
//! the response body.
//!
//! Installed separately by `pond-server` — the same shape as `init_audit_deps`
//! and `init_vision_deps` — rather than threaded through
//! `register_giap_extensions`. Two reasons. The secret repository is built after
//! the agent backend in `serve()`, and the tools only read it at chat time. And
//! there must be exactly ONE instance in the process: `FileSecretRepository`
//! caches `secrets.json` in memory and rewrites it whole on `set`, so a second
//! instance built from the same `data_dir` would serve a stale cache and clobber
//! the API's writes.

use pond_core::security::ports::secret::SecretRepository;
use std::sync::{Arc, OnceLock};

static SECRET_REPO: OnceLock<Arc<dyn SecretRepository + Send + Sync>> = OnceLock::new();

/// Install the secret store. Call once at startup, before any chat session.
pub fn init_secret_deps(repo: Arc<dyn SecretRepository + Send + Sync>) {
    let _ = SECRET_REPO.set(repo);
}

/// Read one secret, or `None`.
///
/// `None` covers three cases and they are deliberately indistinguishable to the
/// caller: the key is unset, the store failed to read, and no entry point ever
/// installed a store (the `chat` and `agent` CLI paths do not). Every caller
/// degrades to its keyless fallback, so on failure a tool reaches for LESS,
/// never more.
pub async fn secret(key: &str) -> Option<String> {
    let repo = SECRET_REPO.get()?;
    match repo.get(key).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(key, error = %e, "secret store read failed; treating the key as unset");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    struct OneKey;

    #[async_trait::async_trait]
    impl SecretRepository for OneKey {
        async fn get(&self, key: &str) -> Result<Option<String>> {
            Ok((key == "GUARDIAN_API_KEY").then(|| "guardian-live-key".to_string()))
        }
        async fn set(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn list_keys(&self) -> Result<Vec<String>> {
            Ok(vec!["GUARDIAN_API_KEY".to_string()])
        }
        async fn has(&self, key: &str) -> Result<bool> {
            Ok(key == "GUARDIAN_API_KEY")
        }
    }

    /// One test, not two: `SECRET_REPO` is a process-global `OnceLock`, so the
    /// before-init and after-init assertions have to share a test or a sibling
    /// running first would decide the outcome. This is the only test in the
    /// crate that calls `init_secret_deps`.
    #[tokio::test]
    async fn an_uninstalled_store_reads_as_unset_and_an_installed_one_reads_through() {
        assert!(
            secret("GUARDIAN_API_KEY").await.is_none(),
            "with no store installed a tool must see no key, so it takes its keyless fallback"
        );

        init_secret_deps(Arc::new(OneKey));

        assert_eq!(
            secret("GUARDIAN_API_KEY").await.as_deref(),
            Some("guardian-live-key")
        );
        assert!(secret("GNEWS_API_KEY").await.is_none());
    }
}
