//! GIAP-specific prompt templates for the Goose agent.
//!
//! Provides home-assistant-flavoured versions of every Goose built-in prompt
//! (`system.md`, `compaction.md`, etc.) plus a new `tiny_model_decision.md`
//! for on-device model routing.
//!
//! ## Usage
//!
//! ```rust
//! use pond_adapters_goose::giap_prompts::GiapPrompts;
//! use std::collections::HashMap;
//!
//! let mut ctx: HashMap<&str, &str> = HashMap::new();
//! ctx.insert("assistant_name", "Goose");
//! ctx.insert("user_name", "Jerry");
//! ctx.insert("personality", "warm and direct");
//! ctx.insert("timezone", "Africa/Nairobi");
//!
//! let rendered = GiapPrompts::render("system.md", &ctx).unwrap();
//! ```
//!
//! ## Template variables
//!
//! Templates use [minijinja](https://docs.rs/minijinja) syntax (same as Goose's
//! built-in templates). Variables are passed as any `serde::Serialize` type.
//!
//! Common variables across templates:
//! - `assistant_name` — the assistant's display name
//! - `user_name` — the primary user's name
//! - `personality` — personality hint (e.g. "warm and direct")
//! - `timezone` — IANA timezone string (e.g. "Africa/Nairobi")
//! - `location` — optional human-readable location (e.g. "Nairobi")
//!
//! See individual template files in `src/prompts/` for per-template variables.

use goose::prompt_template::render_string;
use serde::Serialize;

// ── Embedded template files ───────────────────────────────────────────────────

const SYSTEM:              &str = include_str!("prompts/system.md");
const COMPACTION:          &str = include_str!("prompts/compaction.md");
const SUBAGENT_SYSTEM:     &str = include_str!("prompts/subagent_system.md");
const RECIPE:              &str = include_str!("prompts/recipe.md");
const APPS_CREATE:         &str = include_str!("prompts/apps_create.md");
const APPS_ITERATE:        &str = include_str!("prompts/apps_iterate.md");
const PERMISSION_JUDGE:    &str = include_str!("prompts/permission_judge.md");
const PLAN:                &str = include_str!("prompts/plan.md");
const TINY_MODEL_SYSTEM:   &str = include_str!("prompts/tiny_model_system.md");
const SESSION_NAME:        &str = include_str!("prompts/session_name.md");
const TINY_MODEL_DECISION: &str = include_str!("prompts/tiny_model_decision.md");

/// Registry of GIAP-specific prompt templates.
///
/// Each template is a GIAP-flavoured version of the corresponding Goose
/// built-in prompt, with home assistant identity, safety rules, and voice
/// output constraints baked in. `tiny_model_decision.md` is a new template
/// unique to GIAP for routing tasks to the right local model.
pub struct GiapPrompts;

impl GiapPrompts {
    /// Render a named GIAP prompt template with `context`.
    ///
    /// `context` can be any `serde::Serialize` value — typically a
    /// `HashMap<&str, &str>` or a dedicated context struct.
    ///
    /// Returns an error if the template name is unknown or minijinja rendering
    /// fails (syntax error in a custom override, etc.).
    pub fn render<T: Serialize>(name: &str, context: &T) -> anyhow::Result<String> {
        let template = Self::get(name)
            .ok_or_else(|| anyhow::anyhow!("Unknown GIAP prompt template: '{}'", name))?;
        let rendered = render_string(template, context)
            .map_err(|e| anyhow::anyhow!("Failed to render GIAP prompt '{}': {}", name, e))?;
        Ok(rendered)
    }

    /// Return the raw (un-rendered) template string for `name`, or `None` if
    /// the name is not registered.
    pub fn get(name: &str) -> Option<&'static str> {
        match name {
            "system.md"              => Some(SYSTEM),
            "compaction.md"          => Some(COMPACTION),
            "subagent_system.md"     => Some(SUBAGENT_SYSTEM),
            "recipe.md"              => Some(RECIPE),
            "apps_create.md"         => Some(APPS_CREATE),
            "apps_iterate.md"        => Some(APPS_ITERATE),
            "permission_judge.md"    => Some(PERMISSION_JUDGE),
            "plan.md"                => Some(PLAN),
            "tiny_model_system.md"   => Some(TINY_MODEL_SYSTEM),
            "session_name.md"        => Some(SESSION_NAME),
            "tiny_model_decision.md" => Some(TINY_MODEL_DECISION),
            _ => None,
        }
    }

    /// List all registered GIAP prompt names.
    pub fn list() -> &'static [&'static str] {
        &[
            "system.md",
            "compaction.md",
            "subagent_system.md",
            "recipe.md",
            "apps_create.md",
            "apps_iterate.md",
            "permission_judge.md",
            "plan.md",
            "tiny_model_system.md",
            "session_name.md",
            "tiny_model_decision.md",
        ]
    }

    /// Returns `true` if `name` is a registered GIAP prompt.
    pub fn is_registered(name: &str) -> bool {
        Self::get(name).is_some()
    }
}

// ── Tests ───────────────────────────────────────────────────────────��─────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn base_ctx() -> HashMap<&'static str, &'static str> {
        let mut m = HashMap::new();
        m.insert("assistant_name", "Goose");
        m.insert("user_name", "Jerry");
        m.insert("personality", "warm and direct");
        m.insert("timezone", "Africa/Nairobi");
        m.insert("location", "Nairobi");
        m
    }

    #[test]
    fn list_returns_all_templates() {
        assert_eq!(GiapPrompts::list().len(), 11);
        assert!(GiapPrompts::list().contains(&"tiny_model_decision.md"));
    }

    #[test]
    fn get_returns_none_for_unknown() {
        assert!(GiapPrompts::get("nonexistent.md").is_none());
    }

    #[test]
    fn is_registered_works() {
        assert!(GiapPrompts::is_registered("system.md"));
        assert!(!GiapPrompts::is_registered("unknown.md"));
    }

    #[test]
    fn render_system_substitutes_vars() {
        let ctx = base_ctx();
        let rendered = GiapPrompts::render("system.md", &ctx).unwrap();
        assert!(rendered.contains("Goose"));
        assert!(rendered.contains("Jerry"));
        assert!(rendered.contains("Africa/Nairobi"));
        assert!(rendered.contains("Nairobi"));
    }

    #[test]
    fn render_session_name_has_examples() {
        let ctx: HashMap<&str, &str> = HashMap::new();
        let rendered = GiapPrompts::render("session_name.md", &ctx).unwrap();
        assert!(rendered.contains("weather") || rendered.contains("lights"));
    }

    #[test]
    fn render_tiny_model_decision_contains_routing_rules() {
        let mut ctx: HashMap<&str, &str> = HashMap::new();
        ctx.insert("task_type", "voice_input");
        ctx.insert("task_description", "transcribe user utterance");
        let rendered = GiapPrompts::render("tiny_model_decision.md", &ctx).unwrap();
        assert!(rendered.contains("Whisper"));
        assert!(rendered.contains("selected_model"));
    }

    #[test]
    fn render_compaction_contains_messages_placeholder_when_not_provided() {
        // messages var not provided — minijinja renders it as undefined/empty
        let ctx: HashMap<&str, &str> = HashMap::new();
        // Should not panic; just produce output with missing var handled by minijinja
        let result = GiapPrompts::render("compaction.md", &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn render_unknown_template_returns_error() {
        let ctx: HashMap<&str, &str> = HashMap::new();
        assert!(GiapPrompts::render("does_not_exist.md", &ctx).is_err());
    }

    #[test]
    fn all_templates_render_without_panic() {
        // Smoke test: every registered template should render with an empty context
        let ctx: HashMap<&str, serde_json::Value> = HashMap::new();
        for name in GiapPrompts::list() {
            let result = GiapPrompts::render(name, &ctx);
            assert!(result.is_ok(), "Template '{}' failed to render: {:?}", name, result);
        }
    }
}
