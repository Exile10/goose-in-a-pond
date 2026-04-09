use serde::{Deserialize, Serialize};

/// A user-defined skill injected into the agent's system prompt.
///
/// Skills are Markdown-formatted instruction blocks (e.g. "How to control
/// the lights", "Morning briefing format"). Each active skill is injected
/// as `extend_system_prompt("skill:{name}", content)` on every turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSkill {
    /// UUID primary key.
    pub id: String,
    /// Human-readable name, must be unique (e.g. "light_control", "morning_briefing").
    pub name: String,
    /// Markdown instruction content injected into the system prompt.
    pub content: String,
    /// Whether this skill is currently active.
    pub active: bool,
    /// ISO datetime when this skill was created.
    pub created_at: String,
}
