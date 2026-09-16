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
    /// `true` = the user edited this template; the startup factory reseed
    /// skips it so the edit survives restarts. Cleared by an explicit reset.
    ///
    /// This is the table's analogue of `settings.is_user_set`: written by
    /// exactly one thing, an explicit save from the Prompts tab. It is never
    /// cleared on the user's behalf — see [`Self::factory_version`].
    #[serde(default)]
    pub is_customized: bool,
    /// Which generation of the built-in templates this row was seeded from.
    ///
    /// The reseed stamps [`FACTORY_VERSION`] on rows it owns. A customized row
    /// keeps the generation it was forked from, so
    /// `is_customized && factory_version < FACTORY_VERSION` is exactly "your
    /// edit is based on an older built-in", which the UI can offer to update
    /// rather than silently taking the edit away.
    ///
    /// `0` means the row predates the column.
    #[serde(default)]
    pub factory_version: i64,
    /// ISO datetime of the last update (SQLite `datetime('now')` format).
    pub updated_at: String,
}

/// The current generation of the built-in prompt templates.
///
/// Bump this when the shipped templates change in a way an existing user would
/// want — not for a typo. It is what turns "you edited this once" into "you
/// edited this once, and there is a newer built-in you have not seen".
///
/// 1 is the tag-skeleton rewrite of 2026-08-14: judgment licences for tools and
/// thinking, the covertness rule generalised to any angle-bracket block, and
/// compact tiers for every section that lacked one.
///
/// 2 is 2026-09-10: every tool section — `<tool-usage>`, `<tool-failure>`,
/// `<memory-rules>`, the schema sentence and the tool-vs-memory precedence
/// rule — became conditional on the turn actually being offered tools. A fork
/// of generation 1 still instructs a model with nothing to call, which is
/// ~800 characters of dead prose on every turn.
pub const FACTORY_VERSION: i64 = 2;
