use crate::user_data::domain::skill::UserSkill;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: user skill persistence.
///
/// Each active skill's name + description are injected into the agent's
/// system prompt on every turn; the full `content` is loaded on demand via
/// the `load_skill` tool (see `get_by_name`).
#[async_trait]
pub trait UserSkillRepository: Send + Sync {
    /// Return all active skills, ordered by name.
    async fn list_active(&self) -> Result<Vec<UserSkill>>;

    /// Return all skills (active and inactive), ordered by name.
    async fn list_all(&self) -> Result<Vec<UserSkill>>;

    /// Fetch a skill by its UUID.
    async fn get(&self, id: &str) -> Result<Option<UserSkill>>;

    /// Fetch an active skill by its name. Used by `load_skill` to pull a
    /// skill's full content into context on demand; inactive skills are
    /// invisible to it, matching `list_active`.
    async fn get_by_name(&self, name: &str) -> Result<Option<UserSkill>>;

    /// Insert a new skill. The `id` field must be a UUID set by the caller.
    async fn create(&self, skill: &UserSkill) -> Result<()>;

    /// Update an existing skill's content and/or active flag.
    async fn update(&self, skill: &UserSkill) -> Result<()>;

    /// Delete a skill by its UUID.
    async fn delete(&self, id: &str) -> Result<()>;
}
