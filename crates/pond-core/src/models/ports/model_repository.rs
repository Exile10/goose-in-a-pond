//! Driven port: model catalog and role assignment persistence.

use anyhow::Result;
use async_trait::async_trait;

use crate::domain::model_record::{ModelCategory, ModelRecord, ModelRoleAssignment};

/// Driven port for persisting the model catalog and role assignments.
///
/// Implementations: `SqliteModelRepository` (pond-infra).
/// The model catalog survives restarts; role assignments are the source of truth
/// (the settings KV table is a hot-cache, synced from here on startup).
#[async_trait]
pub trait ModelRepository: Send + Sync {
    // ── Catalog ───────────────────────────────────────────────────────────────

    /// Return all models, ordered by category then name.
    async fn list_all(&self) -> Result<Vec<ModelRecord>>;

    /// Return models filtered to a single category.
    async fn list_by_category(&self, category: &ModelCategory) -> Result<Vec<ModelRecord>>;

    /// Look up a model by its stable `"{category}/{name}"` id.
    async fn get_by_id(&self, id: &str) -> Result<Option<ModelRecord>>;

    /// Insert or replace a model record.
    ///
    /// Uses `INSERT OR REPLACE` semantics. For registry-seeded rows call this;
    /// for user-added rows set `model.is_custom = true` before calling.
    async fn upsert(&self, model: &ModelRecord) -> Result<()>;

    /// Update only the `downloaded` flag (and `updated_at`) for a model.
    async fn set_downloaded(&self, id: &str, downloaded: bool) -> Result<()>;

    // ── Role assignments ──────────────────────────────────────────────────────

    /// Return all current role assignments.
    async fn list_assignments(&self) -> Result<Vec<ModelRoleAssignment>>;

    /// Return the assignment for a specific role, if any.
    async fn get_assignment(&self, role: &str) -> Result<Option<ModelRoleAssignment>>;

    /// Assign a model to a role (INSERT OR REPLACE).
    ///
    /// `role` must be one of `"chat"` | `"think"` | `"task"` | `"asr"` | `"tts"`.
    /// `model_id` must be an existing `models.id`.
    async fn set_assignment(&self, role: &str, model_id: &str) -> Result<()>;

    /// Remove the assignment for a role (no-op if not set).
    async fn clear_assignment(&self, role: &str) -> Result<()>;
}
