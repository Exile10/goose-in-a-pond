use serde::{Deserialize, Serialize};

/// A user-defined skill injected into the agent's system prompt.
///
/// Skills follow the agentskills.io shape: a short `description` sits in the
/// prompt at all times so the model can judge relevance, while the full
/// `content` (instructions) is loaded on demand via the `load_skill` tool
/// rather than injected on every turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSkill {
    /// UUID primary key.
    pub id: String,
    /// Human-readable name, must be unique (e.g. "Light Control", "Morning
    /// Briefing"). Unlike goose's filesystem-backed Agent Skills — where the
    /// name doubles as a directory/slug — this is a DB row addressed by
    /// exact-string lookup (`load_skill`), so it stays free-form rather than
    /// forced into slug shape. See `validate_skill_name`.
    pub name: String,
    /// Short natural-language description of what the skill is for and when it
    /// applies. Always visible to the model, so it must stay small.
    pub description: String,
    /// Markdown instruction content, loaded into context on demand.
    pub content: String,
    /// Whether this skill is currently active.
    pub active: bool,
    /// ISO datetime when this skill was created.
    pub created_at: String,
}

/// Maximum allowed name length (bytes).
pub const MAX_NAME_LEN: usize = 100;

/// Validate a skill name: non-empty (after trimming), at most
/// `MAX_NAME_LEN` bytes, and a single line — no newlines, since it is
/// rendered inline in the skill list the model sees. No charset
/// restriction beyond that: "Task Reminder" is exactly as valid as
/// "task-reminder".
pub fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Skill name must not be empty".to_string());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!(
            "Invalid skill name \"{}\". Names must be at most {} characters.",
            name, MAX_NAME_LEN
        ));
    }
    if name.contains('\n') || name.contains('\r') {
        return Err(format!(
            "Invalid skill name \"{}\". Names must be a single line.",
            name
        ));
    }
    Ok(())
}

impl UserSkill {
    /// Maximum allowed content length (bytes). Loaded on demand via
    /// `load_skill`, but still bounded so one skill can't blow the context
    /// budget of whatever turn loads it.
    pub const MAX_CONTENT_LEN: usize = 5000;

    /// Maximum allowed description length (bytes). Descriptions are injected
    /// into the system prompt on every turn for every active skill, so this
    /// stays small on purpose.
    pub const MAX_DESCRIPTION_LEN: usize = 280;

    /// Validate name format and content/description length.
    pub fn validate(&self) -> Result<(), String> {
        validate_skill_name(&self.name)?;
        if self.description.len() > Self::MAX_DESCRIPTION_LEN {
            return Err(format!(
                "Skill description exceeds {} bytes (got {})",
                Self::MAX_DESCRIPTION_LEN,
                self.description.len()
            ));
        }
        if self.content.len() > Self::MAX_CONTENT_LEN {
            return Err(format!(
                "Skill content exceeds {} bytes (got {})",
                Self::MAX_CONTENT_LEN,
                self.content.len()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_human_readable_title_is_a_valid_name() {
        assert!(validate_skill_name("Task Reminder").is_ok());
        assert!(validate_skill_name("Morning Briefing Assistant").is_ok());
        // Hyphenated slugs remain valid too — nothing forces either style.
        assert!(validate_skill_name("task-reminder").is_ok());
    }

    #[test]
    fn an_empty_or_whitespace_only_name_is_rejected() {
        assert!(validate_skill_name("").is_err());
        assert!(validate_skill_name("   ").is_err());
    }

    #[test]
    fn a_multiline_name_is_rejected() {
        assert!(validate_skill_name("Task\nReminder").is_err());
    }

    #[test]
    fn an_overlong_name_is_rejected() {
        let name = "a".repeat(MAX_NAME_LEN + 1);
        assert!(validate_skill_name(&name).is_err());
    }
}
