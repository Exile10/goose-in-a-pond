use serde::{Deserialize, Serialize};

/// A keyed extra instruction injected into the agent's system prompt.
///
/// Each active extra is pushed to the agent via
/// `agent.extend_system_prompt(key, instruction)` on every turn.
/// Extras are ordered by `sort_order` ascending, then by `key`.
///
/// Use cases: custom home rules, language preference, safety constraints,
/// domain-specific knowledge snippets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptExtra {
    /// Unique key that identifies this instruction block (e.g. "home_rules", "language").
    pub key: String,
    /// The instruction text injected as an extra system prompt section.
    pub instruction: String,
    /// Whether this extra is currently active.
    pub active: bool,
    /// Determines injection order (lower = first). Default 0.
    pub sort_order: i32,
}
