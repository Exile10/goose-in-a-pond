//! Settings port — driven port for persisting and loading GIAP configuration.

use crate::user_data::domain::settings::Settings;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;

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

    /// Persist only the named fields of `settings` (`None` = every field).
    ///
    /// A caller that changes one field should pass just that field's key: writing
    /// a whole snapshot would revert any field another writer (the phone, the
    /// other UI, a model activation) changed after the caller read its copy.
    async fn update_fields(
        &self,
        settings: &Settings,
        _only: Option<&HashSet<String>>,
    ) -> Result<()> {
        self.update(settings).await
    }

    /// Get a single setting by key. Returns `None` if the key is not set.
    async fn get_key(&self, key: &str) -> Result<Option<String>>;

    /// Set a single setting key. Creates the row if it doesn't exist.
    ///
    /// This is the server-side writer (role-assignment sync, model activation,
    /// onboarding): it records a VALUE, never user intent. Use
    /// [`SettingsRepository::mark_user_set`] for that.
    async fn set_key(&self, key: &str, value: String) -> Result<()>;

    /// Delete a single setting row. Deleting an absent key is not an error.
    ///
    /// The default is `Err`, not `Ok(())`, and that is the point. Its only
    /// caller is PAI-2 P2's one-time move of API-key material into the
    /// `SecretRepository`, which deletes the ONLY other copy of a user's key.
    /// A default `Ok(())` would let an adapter that cannot delete report that
    /// it had, and the caller would believe the row was gone. Refusing means
    /// the migration leaves the row in place, which is the safe direction.
    async fn delete_key(&self, _key: &str) -> Result<()> {
        anyhow::bail!("delete_key is not implemented for this SettingsRepository")
    }

    /// Record that the user DELIBERATELY chose these keys.
    ///
    /// A stored row means only "something wrote this key" — server-side
    /// snapshot writers pin every key, so the presence of a row cannot be read
    /// as a choice. The key set of a `PUT /api/v1/settings` patch can: those
    /// are the fields the caller actually sent. Marked keys are exempt from
    /// [`crate::user_data::domain::settings::DEFAULT_ADOPTIONS`], so a future
    /// default change never overwrites a deliberate choice.
    ///
    /// Only marks keys that already have a row; call it after the write.
    /// The default impl is a no-op so test doubles need not carry the flag —
    /// the SQLite adapter is the one that must persist it.
    async fn mark_user_set(&self, _keys: &HashSet<String>) -> Result<()> {
        Ok(())
    }

    /// Whether `key` was deliberately chosen by the user (see
    /// [`SettingsRepository::mark_user_set`]). Unknown keys are not user-set.
    async fn is_user_set(&self, _key: &str) -> Result<bool> {
        Ok(false)
    }
}
