use anyhow::Result;
use async_trait::async_trait;
use pond_core::security::ports::secret::SecretRepository;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;

/// File-based secret store at `$DATA_DIR/secrets.json` with 0o600 permissions.
///
/// Env vars take highest priority: if `std::env::var(key)` succeeds, the file
/// store is skipped entirely. This matches Goose's 3-tier fallback pattern
/// (keyring → file → env) but inverts the priority so explicit env overrides
/// always win — useful for CI and container deployments.
pub struct FileSecretRepository {
    path: PathBuf,
    cache: RwLock<HashMap<String, String>>,
}

impl FileSecretRepository {
    pub fn new(data_dir: &std::path::Path) -> Result<Self> {
        let path = data_dir.join("secrets.json");
        let cache = if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            HashMap::new()
        };
        Ok(Self {
            path,
            cache: RwLock::new(cache),
        })
    }

    async fn persist(&self) -> Result<()> {
        let cache = self.cache.read().await;
        let json = serde_json::to_string_pretty(&*cache)?;
        tokio::fs::write(&self.path, &json).await?;
        // Set file permissions to 0o600 (owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&self.path, perms)?;
        }
        Ok(())
    }
}

#[async_trait]
impl SecretRepository for FileSecretRepository {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        // Check env var first (highest priority)
        if let Ok(val) = std::env::var(key) {
            return Ok(Some(val));
        }
        let cache = self.cache.read().await;
        Ok(cache.get(key).cloned())
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        {
            let mut cache = self.cache.write().await;
            cache.insert(key.to_string(), value.to_string());
        }
        self.persist().await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        {
            let mut cache = self.cache.write().await;
            cache.remove(key);
        }
        self.persist().await
    }

    async fn list_keys(&self) -> Result<Vec<String>> {
        let cache = self.cache.read().await;
        Ok(cache.keys().cloned().collect())
    }

    async fn has(&self, key: &str) -> Result<bool> {
        if std::env::var(key).is_ok() {
            return Ok(true);
        }
        let cache = self.cache.read().await;
        Ok(cache.contains_key(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_get_delete_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FileSecretRepository::new(tmp.path()).unwrap();

        // Initially empty
        assert!(repo.list_keys().await.unwrap().is_empty());
        assert!(!repo.has("MY_KEY").await.unwrap());
        assert!(repo.get("MY_KEY").await.unwrap().is_none());

        // Set a secret
        repo.set("MY_KEY", "secret_value").await.unwrap();
        assert!(repo.has("MY_KEY").await.unwrap());
        assert_eq!(
            repo.get("MY_KEY").await.unwrap().as_deref(),
            Some("secret_value")
        );
        assert_eq!(repo.list_keys().await.unwrap(), vec!["MY_KEY".to_string()]);

        // Delete it
        repo.delete("MY_KEY").await.unwrap();
        assert!(!repo.has("MY_KEY").await.unwrap());
        assert!(repo.get("MY_KEY").await.unwrap().is_none());
        assert!(repo.list_keys().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn overwrite_existing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FileSecretRepository::new(tmp.path()).unwrap();

        repo.set("TOKEN", "old").await.unwrap();
        repo.set("TOKEN", "new").await.unwrap();
        assert_eq!(repo.get("TOKEN").await.unwrap().as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn persists_to_disk() {
        let tmp = tempfile::tempdir().unwrap();

        // Write with one instance
        {
            let repo = FileSecretRepository::new(tmp.path()).unwrap();
            repo.set("PERSIST_KEY", "persist_val").await.unwrap();
        }

        // Read with a fresh instance
        let repo2 = FileSecretRepository::new(tmp.path()).unwrap();
        assert_eq!(
            repo2.get("PERSIST_KEY").await.unwrap().as_deref(),
            Some("persist_val")
        );
    }

    #[tokio::test]
    async fn delete_nonexistent_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = FileSecretRepository::new(tmp.path()).unwrap();
        // Should not error
        repo.delete("DOES_NOT_EXIST").await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_permissions_are_0600() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let repo = FileSecretRepository::new(tmp.path()).unwrap();
        repo.set("KEY", "val").await.unwrap();

        let meta = std::fs::metadata(tmp.path().join("secrets.json")).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secrets.json should be owner-only rw");
    }
}
