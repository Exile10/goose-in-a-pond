use crate::domain::prompt_extra::PromptExtra;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: system prompt extras persistence.
///
/// Extras are keyed instruction blocks injected into every agent turn via
/// `agent.extend_system_prompt(key, instruction)`. They are ordered by
/// `sort_order ASC, key ASC`.
#[async_trait]
pub trait PromptExtraRepository: Send + Sync {
    /// Return all active extras, in injection order.
    async fn list_active(&self) -> Result<Vec<PromptExtra>>;

    /// Return all extras (active and inactive).
    async fn list_all(&self) -> Result<Vec<PromptExtra>>;

    /// Insert or replace an extra (keyed by `key`).
    async fn upsert(&self, extra: &PromptExtra) -> Result<()>;

    /// Delete an extra by key.
    async fn delete(&self, key: &str) -> Result<()>;
}
