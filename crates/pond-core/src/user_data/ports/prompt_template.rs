use crate::user_data::domain::prompt_template::PromptTemplate;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: prompt template persistence.
///
/// Templates are named system prompt strings stored in the DB and editable
/// by the user at runtime. Built-in templates (`is_system = true`) are seeded
/// at `run_setup()` using `INSERT OR IGNORE` (never overwrites user edits).
#[async_trait]
pub trait PromptTemplateRepository: Send + Sync {
    /// Fetch a template by name. Returns `None` if not found.
    async fn get(&self, name: &str) -> Result<Option<PromptTemplate>>;

    /// List all templates, ordered by name.
    async fn list(&self) -> Result<Vec<PromptTemplate>>;

    /// Insert or replace a template record.
    ///
    /// When seeding defaults at setup, check `INSERT OR IGNORE` logic in the
    /// caller (`run_setup`) — do not call `upsert` unconditionally for seeding.
    async fn upsert(&self, template: &PromptTemplate) -> Result<()>;

    /// Insert a template only if no row with that name exists.
    /// Returns `true` if the row was inserted, `false` if it already existed.
    async fn insert_if_absent(&self, template: &PromptTemplate) -> Result<bool>;

    /// Delete a template by name.
    ///
    /// Callers should check `is_system` before calling — system templates
    /// should not be deleted via the public API.
    async fn delete(&self, name: &str) -> Result<()>;
}
