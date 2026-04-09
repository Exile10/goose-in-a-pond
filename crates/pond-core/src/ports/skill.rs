use crate::domain::skill::UserSkill;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: user skill persistence.
///
/// Skills are Markdown-formatted instruction blocks injected into the agent's
/// system prompt via `extend_system_prompt("skill:{name}", content)`.
#[async_trait]
pub trait UserSkillRepository: Send + Sync {
    /// Return all active skills, ordered by name.
    async fn list_active(&self) -> Result<Vec<UserSkill>>;

    /// Return all skills (active and inactive), ordered by name.
    async fn list_all(&self) -> Result<Vec<UserSkill>>;

    /// Fetch a skill by its UUID.
    async fn get(&self, id: &str) -> Result<Option<UserSkill>>;

    /// Insert a new skill. The `id` field must be a UUID set by the caller.
    async fn create(&self, skill: &UserSkill) -> Result<()>;

    /// Update an existing skill's content and/or active flag.
    async fn update(&self, skill: &UserSkill) -> Result<()>;

    /// Delete a skill by its UUID.
    async fn delete(&self, id: &str) -> Result<()>;
}
