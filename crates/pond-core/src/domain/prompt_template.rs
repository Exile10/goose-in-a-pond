use serde::{Deserialize, Serialize};

/// A named system prompt template stored in the database.
///
/// Templates support minijinja-style `{{variable}}` placeholders:
/// `{{assistant_name}}`, `{{user_name}}`, `{{personality}}`,
/// `{{timezone}}`, `{{location}}`, `{{prompt_addendum}}`.
///
/// System templates (`is_system = true`) are seeded at `run_setup()` and
/// cannot be deleted via the API, but their content is always editable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplate {
    /// Unique name / slug — e.g. "balanced", "concise", or a user-defined name.
    pub name: String,
    /// Full template content with optional `{{placeholders}}`.
    pub content: String,
    /// Human-readable description shown in the UI.
    pub description: String,
    /// `true` = built-in template seeded at setup; cannot be deleted.
    pub is_system: bool,
    /// ISO datetime of the last update (SQLite `datetime('now')` format).
    pub updated_at: String,
}
