//! Settings port — driven port for persisting and loading GIAP configuration.

use crate::domain::settings::Settings;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: Settings persistence.
///
/// Settings are stored as flat key-value pairs (one row per field).
/// Missing keys fall back to the `Settings::default()` value for that field.
#[async_trait]
pub trait SettingsRepository: Send + Sync {
    /// Load all settings, applying defaults for any keys absent from the store.
    async fn get(&self) -> Result<Settings>;

    /// Persist an entire `Settings` struct (upsert every field).
    async fn update(&self, settings: &Settings) -> Result<()>;

    /// Get a single setting by key. Returns `None` if the key is not set.
    async fn get_key(&self, key: &str) -> Result<Option<String>>;

    /// Set a single setting key. Creates the row if it doesn't exist.
    async fn set_key(&self, key: &str, value: String) -> Result<()>;
}
