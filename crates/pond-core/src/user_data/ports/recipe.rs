use crate::user_data::domain::recipe::AgentRecipe;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: agent recipe persistence.
///
/// Recipes are Goose-compatible YAML automations stored in the DB.
/// They are executed via `POST /api/v1/recipes/{name}/run` — there is no
/// MCP tool for self-invocation by the agent; a recipe run is always
/// caller-initiated.
#[async_trait]
pub trait AgentRecipeRepository: Send + Sync {
    /// Return all recipes, ordered by name.
    async fn list(&self) -> Result<Vec<AgentRecipe>>;

    /// Fetch a recipe by its unique name/slug.
    async fn get_by_name(&self, name: &str) -> Result<Option<AgentRecipe>>;

    /// Fetch a recipe by its UUID.
    async fn get_by_id(&self, id: &str) -> Result<Option<AgentRecipe>>;

    /// Insert or replace a recipe. `name` must be unique.
    async fn upsert(&self, recipe: &AgentRecipe) -> Result<()>;

    /// Delete a recipe by UUID.
    async fn delete(&self, id: &str) -> Result<()>;
}
