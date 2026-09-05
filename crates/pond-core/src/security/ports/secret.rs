use anyhow::Result;
use async_trait::async_trait;

/// Secure storage for extension secrets (API keys, OAuth tokens, etc.).
///
/// Stored in the system keyring (macOS Keychain, Linux secret-service) with a file
/// fallback for headless environments. Values are NEVER returned through the REST API.
#[async_trait]
pub trait SecretRepository: Send + Sync {
    /// Get a secret value by key. Returns None if not set.
    async fn get(&self, key: &str) -> Result<Option<String>>;
    /// Set a secret value. Overwrites if exists.
    async fn set(&self, key: &str, value: &str) -> Result<()>;
    /// Delete a secret. No-op if not found.
    async fn delete(&self, key: &str) -> Result<()>;
    /// List all stored secret key names (never values).
    async fn list_keys(&self) -> Result<Vec<String>>;
    /// Check if a secret exists without retrieving its value.
    async fn has(&self, key: &str) -> Result<bool>;
}
