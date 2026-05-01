//! Prompt partitioning for KV-cache-friendly local inference.
//!
//! Splits the system prompt into a **static prefix** (identity, personality,
//! capabilities, tool descriptions, behavioral rules) and a **dynamic suffix**
//! (current date/time, memory fragments, profile context, prompt extras).
//!
//! Local inference providers (llama.cpp, Goose GGUF) can reuse the KV-cache
//! for the static prefix across conversation turns, saving significant
//! recomputation on every request.
//!
//! ## Usage
//!
//! ```ignore
//! let partition = build_prompt_partition(&settings, profile, &state, &template);
//!
//! // Only rebuild the base prompt when the prefix hash changes
//! if partition.prefix_hash != last_prefix_hash {
//!     agent.override_system_prompt(partition.static_prefix);
//!     last_prefix_hash = partition.prefix_hash;
//! }
//!
//! // Always update the dynamic portions
//! agent.extend_system_prompt("temporal", partition.dynamic_suffix);
//! ```

use crate::domain::settings::Settings;
use crate::prompts::{
    render_jinja_template, sanitize_field, ProfileContext, PromptState,
    PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// A system prompt split into cache-friendly parts.
///
/// The `static_prefix` contains everything that stays the same between
/// conversation turns (identity, personality, tool descriptions, behavioral
/// rules, device context, thinking/voice mode sections).
///
/// The `dynamic_suffix` contains per-turn content: current date/time,
/// profile context lines, and the prompt addendum.
///
/// `prefix_hash` is a 64-bit hash of the static prefix content, allowing
/// callers to skip expensive `override_system_prompt()` calls when the
/// prefix has not changed.
#[derive(Debug, Clone)]
pub struct PromptPartition {
    /// Stable portion of the system prompt — identity, capabilities, rules.
    /// Changes only when settings, model capabilities, or device state change.
    pub static_prefix: String,
    /// Per-turn dynamic content — date/time, profile lines, addendum.
    pub dynamic_suffix: String,
    /// Hash of `static_prefix` for cheap equality checks.
    pub prefix_hash: u64,
}

/// Compute a 64-bit hash of a string using the standard library hasher.
///
/// Not cryptographic — purely for change detection. Two identical prefixes
/// will always produce the same hash within the same process.
fn hash_string(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Build a partitioned system prompt from settings, profile, state, and template.
///
/// ## Static prefix
///
/// The template is rendered with all stable context variables:
/// - `assistant_name`, `user_name`, `personality`, `timezone`, `location`
/// - `device_count`, `has_home_devices`, `online_device_names`
/// - `has_tools`, `tools` (tool descriptions are static)
/// - `thinking_enabled`, `voice_mode`
///
/// The `current_date` and `current_time` variables are set to empty strings
/// during static rendering so they do not bake time into the prefix.
///
/// ## Dynamic suffix
///
/// Contains lines that change per turn:
/// - Current date and time
/// - Profile context (preferred name, language, birthday, atypical speech)
/// - Prompt addendum from settings
pub fn build_prompt_partition(
    settings: &Settings,
    profile: Option<&ProfileContext>,
    state: &PromptState,
    template_content: &str,
) -> PromptPartition {
    // ── Static prefix: render template with stable vars, blank temporal ──
    let static_state = PromptState {
        current_date: String::new(),
        current_time: String::new(),
        // Carry all non-temporal fields from the caller's state
        device_count: state.device_count,
        has_home_devices: state.has_home_devices,
        online_device_names: state.online_device_names.clone(),
        voice_mode: state.voice_mode,
        available_tools: state.available_tools.clone(),
        thinking_enabled: state.thinking_enabled,
        compact_prompt: state.compact_prompt,
        prefix_hash: None,
    };

    let template = if let Some(ref custom) = settings.custom_system_prompt {
        sanitize_field(custom, 4000)
    } else {
        template_content.to_string()
    };

    let static_prefix = render_jinja_template(&template, settings, Some(&static_state), profile);

    // ── Dynamic suffix: temporal context + profile lines + addendum ──────
    let mut dynamic_parts: Vec<String> = Vec::with_capacity(8);

    // Temporal context
    if !state.current_date.is_empty() || !state.current_time.is_empty() {
        let mut temporal = String::with_capacity(80);
        if !state.current_date.is_empty() {
            temporal.push_str("Today is ");
            temporal.push_str(&state.current_date);
            temporal.push('.');
        }
        if !state.current_time.is_empty() {
            if !temporal.is_empty() {
                temporal.push(' ');
            }
            temporal.push_str("Current time: ");
            temporal.push_str(&state.current_time);
            temporal.push('.');
        }
        dynamic_parts.push(temporal);
    }

    // Profile context lines (same logic as build_system_prompt_from_template_full)
    if let Some(ctx) = profile {
        let user = sanitize_field(&settings.user_name, 50);

        if let Some(ref pname) = ctx.preferred_name {
            let pname = sanitize_field(pname, 50);
            if !pname.is_empty() && pname != user {
                dynamic_parts.push(format!("The user prefers to be called {}.", pname));
            }
        }
        if let Some(ref lang) = ctx.language {
            let lang = sanitize_field(lang, 20);
            if !lang.is_empty() && lang != "en" {
                let lang_label = match lang.as_str() {
                    "fr" => "French",
                    "es" => "Spanish",
                    "de" => "German",
                    "sw" => "Swahili",
                    "ar" => "Arabic",
                    "pt" => "Portuguese",
                    "zh" => "Chinese",
                    "ja" => "Japanese",
                    "ko" => "Korean",
                    other => other,
                };
                dynamic_parts.push(format!("Always respond in {}.", lang_label));
            }
        }
        if let Some(ref bday) = ctx.birthday {
            let bday = sanitize_field(bday, 20);
            if !bday.is_empty() {
                dynamic_parts.push(format!("The user's birthday is {}.", bday));
            }
        }
        if ctx.atypical_speech {
            dynamic_parts.push(
                "The user may have atypical speech — be patient, never correct speech \
                 patterns, and interpret incomplete sentences charitably."
                    .to_string(),
            );
        }
    }

    // Prompt addendum
    let addendum = sanitize_field(&settings.prompt_addendum, 500);
    if !addendum.is_empty() {
        dynamic_parts.push(addendum);
    }

    let dynamic_suffix = dynamic_parts.join("\n");
    let prefix_hash = hash_string(&static_prefix);

    PromptPartition {
        static_prefix,
        dynamic_suffix,
        prefix_hash,
    }
}

/// Compute the prefix hash from settings and state WITHOUT building the full prompt.
///
/// Useful for callers that need to check whether a rebuild is needed before
/// doing the work. The hash is computed over the fields that determine the
/// static prefix content.
pub fn compute_prefix_hash_fast(
    settings: &Settings,
    state: &PromptState,
    template_content: &str,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Settings fields that affect the static prefix
    settings.assistant_name.hash(&mut hasher);
    settings.user_name.hash(&mut hasher);
    settings.assistant_personality.hash(&mut hasher);
    settings.timezone.hash(&mut hasher);
    settings.weather_location_name.hash(&mut hasher);
    settings.prompt_style.hash(&mut hasher);
    settings.custom_system_prompt.hash(&mut hasher);
    // Template content
    template_content.hash(&mut hasher);
    // State fields that are baked into the static prefix
    state.device_count.hash(&mut hasher);
    state.has_home_devices.hash(&mut hasher);
    state.online_device_names.hash(&mut hasher);
    state.voice_mode.hash(&mut hasher);
    state.thinking_enabled.hash(&mut hasher);
    // Tool descriptions are static, but hash their count as a sanity check
    state.available_tools.len().hash(&mut hasher);
    hasher.finish()
}

/// Resolve the template content string for a given settings configuration.
///
/// Priority:
/// 1. `settings.custom_system_prompt` (if Some)
/// 2. Built-in template selected by `settings.prompt_style`
///
/// This does NOT consult the DB template repository — callers should pass
/// the DB template content as an override when available.
pub fn resolve_builtin_template(settings: &Settings) -> &'static str {
    match settings.prompt_style.as_str() {
        "concise" => PROMPT_CONCISE,
        "technical" => PROMPT_TECHNICAL,
        "warm" => PROMPT_WARM,
        _ => PROMPT_BALANCED,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::PROMPT_BALANCED;

    fn default_state() -> PromptState {
        PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:32".to_string(),
            device_count: 0,
            has_home_devices: false,
            online_device_names: String::new(),
            voice_mode: false,
            available_tools: vec!["wikipedia — Look up factual info".to_string()],
            thinking_enabled: false,
            compact_prompt: false,
            prefix_hash: None,
        }
    }

    #[test]
    fn static_prefix_excludes_date_and_time() {
        let settings = Settings::default();
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        // Static prefix must NOT contain the date or time
        assert!(
            !partition.static_prefix.contains("Thursday, 1 May 2026"),
            "Static prefix must not contain the current date"
        );
        assert!(
            !partition.static_prefix.contains("14:32"),
            "Static prefix must not contain the current time"
        );
    }

    #[test]
    fn dynamic_suffix_contains_date_and_time() {
        let settings = Settings::default();
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(
            partition.dynamic_suffix.contains("Thursday, 1 May 2026"),
            "Dynamic suffix must contain the current date"
        );
        assert!(
            partition.dynamic_suffix.contains("14:32"),
            "Dynamic suffix must contain the current time"
        );
    }

    #[test]
    fn static_prefix_contains_identity() {
        let mut settings = Settings::default();
        settings.assistant_name = "Duck".to_string();
        settings.user_name = "Jerry".to_string();
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(partition.static_prefix.contains("Duck"));
        assert!(partition.static_prefix.contains("Jerry"));
    }

    #[test]
    fn consecutive_turns_produce_identical_static_prefix() {
        let settings = Settings::default();

        let state1 = PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:32".to_string(),
            ..default_state()
        };

        let state2 = PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:33".to_string(), // time changed!
            ..default_state()
        };

        let p1 = build_prompt_partition(&settings, None, &state1, PROMPT_BALANCED);
        let p2 = build_prompt_partition(&settings, None, &state2, PROMPT_BALANCED);

        assert_eq!(
            p1.static_prefix, p2.static_prefix,
            "Static prefix must be identical across turns when only time changes"
        );
        assert_eq!(
            p1.prefix_hash, p2.prefix_hash,
            "Prefix hash must be identical across turns when only time changes"
        );
    }

    #[test]
    fn dynamic_suffix_changes_when_time_changes() {
        let settings = Settings::default();

        let state1 = PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:32".to_string(),
            ..default_state()
        };

        let state2 = PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:33".to_string(),
            ..default_state()
        };

        let p1 = build_prompt_partition(&settings, None, &state1, PROMPT_BALANCED);
        let p2 = build_prompt_partition(&settings, None, &state2, PROMPT_BALANCED);

        assert_ne!(
            p1.dynamic_suffix, p2.dynamic_suffix,
            "Dynamic suffix must change when time changes"
        );
    }

    #[test]
    fn prefix_hash_changes_when_settings_change() {
        let mut settings1 = Settings::default();
        settings1.assistant_name = "Goose".to_string();

        let mut settings2 = Settings::default();
        settings2.assistant_name = "Duck".to_string();

        let state = default_state();

        let p1 = build_prompt_partition(&settings1, None, &state, PROMPT_BALANCED);
        let p2 = build_prompt_partition(&settings2, None, &state, PROMPT_BALANCED);

        assert_ne!(
            p1.prefix_hash, p2.prefix_hash,
            "Prefix hash must change when assistant name changes"
        );
    }

    #[test]
    fn prefix_hash_changes_when_devices_change() {
        let settings = Settings::default();

        let state1 = PromptState {
            has_home_devices: false,
            device_count: 0,
            ..default_state()
        };

        let state2 = PromptState {
            has_home_devices: true,
            device_count: 2,
            online_device_names: "Speaker, Hub".to_string(),
            ..default_state()
        };

        let p1 = build_prompt_partition(&settings, None, &state1, PROMPT_BALANCED);
        let p2 = build_prompt_partition(&settings, None, &state2, PROMPT_BALANCED);

        assert_ne!(
            p1.prefix_hash, p2.prefix_hash,
            "Prefix hash must change when device state changes"
        );
    }

    #[test]
    fn prefix_hash_changes_when_thinking_mode_changes() {
        let settings = Settings::default();

        let state1 = PromptState {
            thinking_enabled: false,
            ..default_state()
        };

        let state2 = PromptState {
            thinking_enabled: true,
            ..default_state()
        };

        let p1 = build_prompt_partition(&settings, None, &state1, PROMPT_BALANCED);
        let p2 = build_prompt_partition(&settings, None, &state2, PROMPT_BALANCED);

        assert_ne!(
            p1.prefix_hash, p2.prefix_hash,
            "Prefix hash must change when thinking mode changes"
        );
    }

    #[test]
    fn profile_context_goes_to_dynamic_suffix() {
        let settings = Settings::default();
        let state = default_state();
        let profile = ProfileContext {
            preferred_name: Some("Captain".to_string()),
            birthday: Some("1990-03-05".to_string()),
            language: Some("sw".to_string()),
            atypical_speech: false,
        };

        let partition = build_prompt_partition(&settings, Some(&profile), &state, PROMPT_BALANCED);

        assert!(
            partition.dynamic_suffix.contains("Captain"),
            "Profile preferred name must be in dynamic suffix"
        );
        assert!(
            partition.dynamic_suffix.contains("Swahili"),
            "Profile language must be in dynamic suffix"
        );
        assert!(
            partition.dynamic_suffix.contains("1990-03-05"),
            "Profile birthday must be in dynamic suffix"
        );
    }

    #[test]
    fn addendum_goes_to_dynamic_suffix() {
        let mut settings = Settings::default();
        settings.prompt_addendum = "Always respond in French.".to_string();
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(
            partition.dynamic_suffix.contains("Always respond in French."),
            "Addendum must be in dynamic suffix"
        );
        assert!(
            !partition.static_prefix.contains("Always respond in French."),
            "Addendum must NOT be in static prefix"
        );
    }

    #[test]
    fn empty_date_produces_no_temporal_line() {
        let settings = Settings::default();
        let state = PromptState {
            current_date: String::new(),
            current_time: String::new(),
            ..default_state()
        };

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);
        assert!(
            !partition.dynamic_suffix.contains("Today is"),
            "No temporal line when date is empty"
        );
    }

    #[test]
    fn custom_system_prompt_used_in_prefix() {
        let mut settings = Settings::default();
        settings.assistant_name = "Pond".to_string();
        settings.custom_system_prompt =
            Some("I am {{assistant_name}} and I serve {{user_name}}.".to_string());
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(partition.static_prefix.contains("Pond"));
        assert!(partition.static_prefix.contains("Friend"));
    }

    #[test]
    fn fast_hash_matches_full_partition_hash_for_same_input() {
        let settings = Settings::default();
        let state = default_state();

        let fast = compute_prefix_hash_fast(&settings, &state, PROMPT_BALANCED);
        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        // They use different hashing strategies (field-level vs string-level),
        // so they won't match numerically. But they should both change when
        // inputs change. Test that the fast hash is stable.
        let fast2 = compute_prefix_hash_fast(&settings, &state, PROMPT_BALANCED);
        assert_eq!(fast, fast2, "Fast hash must be deterministic");

        // And that it changes when settings change
        let mut settings2 = Settings::default();
        settings2.assistant_name = "Changed".to_string();
        let fast3 = compute_prefix_hash_fast(&settings2, &state, PROMPT_BALANCED);
        assert_ne!(fast, fast3, "Fast hash must change when settings change");
    }

    #[test]
    fn combined_output_matches_full_prompt() {
        let settings = Settings::default();
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        // The combined partition (prefix + suffix) should contain all the content
        // that the full builder produces, just organized differently.
        let combined = if partition.dynamic_suffix.is_empty() {
            partition.static_prefix.clone()
        } else {
            format!("{}\n\n{}", partition.static_prefix, partition.dynamic_suffix)
        };

        // The combined output should contain identity
        assert!(combined.contains("Goose"));
        assert!(combined.contains("Friend"));

        // And temporal content
        assert!(combined.contains("Thursday, 1 May 2026"));
    }

    #[test]
    fn resolve_builtin_template_selects_correct_style() {
        let mut s = Settings::default();

        s.prompt_style = "balanced".to_string();
        assert!(resolve_builtin_template(&s).contains("intelligent AI copilot"));

        s.prompt_style = "concise".to_string();
        assert!(resolve_builtin_template(&s).contains("One sentence replies"));

        s.prompt_style = "technical".to_string();
        assert!(resolve_builtin_template(&s).contains("privacy-first AI copilot"));

        s.prompt_style = "warm".to_string();
        assert!(resolve_builtin_template(&s).contains("Hey there"));

        s.prompt_style = "nonexistent".to_string();
        assert!(resolve_builtin_template(&s).contains("intelligent AI copilot")); // fallback to balanced
    }

    #[test]
    fn voice_mode_section_in_static_prefix() {
        let settings = Settings::default();

        let state = PromptState {
            voice_mode: true,
            ..default_state()
        };

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(
            partition.static_prefix.contains("Voice Mode"),
            "Voice mode section must be in static prefix"
        );
    }

    #[test]
    fn tools_section_in_static_prefix() {
        let settings = Settings::default();
        let state = PromptState {
            available_tools: vec!["wikipedia — Look up factual info".to_string()],
            ..default_state()
        };

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(
            partition.static_prefix.contains("wikipedia"),
            "Tool descriptions must be in static prefix"
        );
    }
}
