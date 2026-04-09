use serde::{Deserialize, Serialize};

/// A saved Goose Recipe stored in the database.
///
/// Recipes are reusable automations expressed as Goose YAML. The agent
/// can execute them on demand via the `giap__run_recipe` MCP tool or
/// via `POST /api/v1/recipes/{name}/run`.
///
/// Minimal recipe YAML:
/// ```yaml
/// title: Morning Brief
/// description: Daily weather and schedule summary
/// prompt: Give me the weather and list any scheduled tasks for today.
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecipe {
    /// UUID primary key.
    pub id: String,
    /// Unique slug used in API paths (e.g. "morning_brief", "lock_doors").
    pub name: String,
    /// One-sentence description shown in the UI.
    pub description: String,
    /// Goose Recipe YAML content.
    pub yaml: String,
    /// Whether this recipe is enabled.
    pub active: bool,
    /// ISO datetime when this recipe was created.
    pub created_at: String,
}
