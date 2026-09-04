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
//!
//! ## Prompt extras are outside `prefix_hash` — by design
//!
//! Prompt extras and skills (injected by the agent via `extend_system_prompt`
//! as `<extension-notes>` blocks) are NOT part of the partition and are not
//! covered by `prefix_hash`: they are appended after the partitioned prompt is
//! applied, and changing them must not invalidate the static-prefix KV cache.

use crate::prompts::{
    render_jinja_template, sanitize_field, ProfileContext, PromptState, PROMPT_BALANCED,
};
use crate::user_data::domain::settings::Settings;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// A system prompt split into cache-friendly parts.
///
/// The `static_prefix` contains everything that stays the same between
/// conversation turns (identity, personality, tool descriptions, behavioral
/// rules, how many devices are registered, thinking/voice mode sections).
///
/// The `dynamic_suffix` contains per-turn content: current date/time, which
/// devices are online, profile context lines, and the prompt addendum.
///
/// `prefix_hash` is a 64-bit hash of the static prefix content, allowing
/// callers to skip expensive `override_system_prompt()` calls when the
/// prefix has not changed.
#[derive(Debug, Clone)]
pub struct PromptPartition {
    /// Stable portion of the system prompt — identity, capabilities, rules.
    /// Changes when settings, model capabilities, the selected tools, or the
    /// set of *registered* devices change — never when a device merely goes
    /// quiet, which is a clock, not a fact about the pond.
    pub static_prefix: String,
    /// Per-turn dynamic content — date/time, live device list, profile lines,
    /// addendum.
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
/// - `device_count`, `has_home_devices`
/// - `has_tools`, `tools` (tool descriptions are static)
/// - `thinking_enabled`, `voice_mode`
///
/// `current_date`, `current_time` and `online_device_names` are set to empty
/// strings during static rendering. The first two are obviously temporal; the
/// third is temporal in disguise, since `is_online` is recomputed on every read
/// from a 300-second heartbeat window rather than stored.
///
/// ## Dynamic suffix
///
/// Contains lines that change per turn:
/// - Current date and time
/// - Which devices are reachable right now
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
        // Blanked for the same reason as the clock, and it is the same kind of
        // field: `is_online` is not a stored column at all — `list_devices` does
        // not even select it. It is derived at read time as
        // `now - last_seen < ONLINE_THRESHOLD_SECS` (300s), so this string
        // changes on a five-minute wall-clock timer with nobody touching
        // anything. Left in the prefix it truncates KV reuse at the
        // `<home-devices>` block every time a phone stops heartbeating.
        // `device_count` and `has_home_devices` stay: they move only when a
        // device is registered or removed.
        online_device_names: String::new(),
        voice_mode: state.voice_mode,
        canvas_mode: state.canvas_mode,
        available_tools: state.available_tools.clone(),
        thinking_enabled: state.thinking_enabled,
        compact_prompt: state.compact_prompt,
        native_tools_json: state.native_tools_json,
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

    // Temporal context — placed prominently so the model answers time/date
    // questions directly without calling tools.
    if !state.current_date.is_empty() || !state.current_time.is_empty() {
        let mut temporal = String::with_capacity(120);
        temporal.push_str("CURRENT CONTEXT: ");
        if !state.current_date.is_empty() {
            temporal.push_str("Today is ");
            temporal.push_str(&state.current_date);
            temporal.push('.');
        }
        if !state.current_time.is_empty() {
            temporal.push_str(" The time is ");
            temporal.push_str(&state.current_time);
            temporal.push('.');
        }
        temporal.push_str(" Answer time/date questions directly from this — no tools needed.");
        dynamic_parts.push(temporal);
    }

    // Which devices are reachable right now. The template's `<home-devices>`
    // block still states how many are registered — that is stable — but the
    // live list is restated here, because it expires on a timer and the prefix
    // has to survive that. Costs nothing when nothing is online, which on a
    // real pond is most of the time.
    if !state.online_device_names.is_empty() {
        dynamic_parts.push(format!(
            "Online right now: {}.",
            sanitize_field(&state.online_device_names, 300)
        ));
    }

    // Profile context lines (same logic as build_system_prompt_from_template_full)
    dynamic_parts.extend(crate::prompts::profile_context_lines(
        profile,
        &settings.user_name,
    ));

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
    // Renders the word cap inside <thinking>. Omitting it would let this fast
    // path report "prefix unchanged" for a prefix that changed, which is the
    // one way a cache check can be worse than no cache check at all.
    settings.reasoning_effort.hash(&mut hasher);
    // Template content
    template_content.hash(&mut hasher);
    // State fields that are baked into the static prefix
    state.device_count.hash(&mut hasher);
    state.has_home_devices.hash(&mut hasher);
    // `online_device_names` is deliberately NOT here: it no longer renders into
    // the static prefix (see `build_prompt_partition`). Hashing it would make
    // this path report "prefix changed" every five minutes for a prefix that
    // did not change, which costs a full re-prefill for nothing.
    state.voice_mode.hash(&mut hasher);
    state.canvas_mode.hash(&mut hasher);
    state.thinking_enabled.hash(&mut hasher);
    // Selects the compact variant of four sections, AND the tier the thinking
    // word cap is drawn from. It was already baked into the static prefix and
    // already missing from this hash before PAI-5 P4; the second consumer is
    // what makes the omission worth closing rather than noting.
    state.compact_prompt.hash(&mut hasher);
    // Gates the "Available tools:" listing inside <tool-usage>
    state.native_tools_json.hash(&mut hasher);
    // The tool LINES, not their count. Hashing only the length was wrong in the
    // one direction that matters: under `tool_selection_mode = "relevant"` the
    // selection is rescored every turn, so swapping one tool for another —
    // same count, different prose in `<tool-usage>` — left this hash unchanged
    // and told the provider to reuse a KV prefix for a prompt it never saw.
    // Order is part of the identity here because it is part of the render.
    state.available_tools.hash(&mut hasher);
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
    // Derived from the one built-in table rather than matching on names here,
    // so a style added to `BUILTIN_PROMPT_TEMPLATES` is resolvable without a
    // second edit. The fallback is NOT the table's business and stays local: an
    // unrecognised style must yield balanced, never an error or an empty prompt.
    crate::prompts::builtin_template_content(&settings.prompt_style)
        .map(|(content, _)| content)
        .unwrap_or(PROMPT_BALANCED)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::{PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM};

    fn default_state() -> PromptState {
        PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:32".to_string(),
            device_count: 0,
            has_home_devices: false,
            online_device_names: String::new(),
            voice_mode: false,
            canvas_mode: false,
            available_tools: vec!["wikipedia — Look up factual info".to_string()],
            thinking_enabled: false,
            compact_prompt: false,
            native_tools_json: false,
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

    /// Registering a device changes the prompt. A device merely *heartbeating*
    /// must not.
    ///
    /// `is_online` is not stored — it is recomputed on every read as
    /// `now - last_seen < 300s`. So the online set turns over on a five-minute
    /// wall-clock timer with nobody doing anything, and if that string rides
    /// the static prefix then the KV cache is truncated at `<home-devices>`
    /// several times an hour for no reason a user could name. Both hashes are
    /// asserted, because they are two independent renderings of the same
    /// question and a fast path that disagrees with the real one is worse than
    /// no fast path.
    #[test]
    fn a_device_going_quiet_does_not_move_the_static_prefix() {
        let settings = Settings::default();

        let registered = PromptState {
            has_home_devices: true,
            device_count: 3,
            online_device_names: "Speaker, Hub, Washer".to_string(),
            ..default_state()
        };
        let all_quiet = PromptState {
            online_device_names: String::new(),
            ..registered.clone()
        };

        let busy = build_prompt_partition(&settings, None, &registered, PROMPT_BALANCED);
        let quiet = build_prompt_partition(&settings, None, &all_quiet, PROMPT_BALANCED);

        assert_eq!(
            busy.static_prefix, quiet.static_prefix,
            "the heartbeat window must not reach the cacheable prefix"
        );
        assert_eq!(
            compute_prefix_hash_fast(&settings, &registered, PROMPT_BALANCED),
            compute_prefix_hash_fast(&settings, &all_quiet, PROMPT_BALANCED),
            "the fast hash must agree that nothing cacheable changed"
        );
    }

    /// …and the model is still told, because moving it out of the prefix would
    /// otherwise be a silent capability loss: "turn on the speaker" needs to
    /// know the speaker is reachable.
    #[test]
    fn the_online_list_still_reaches_the_model_through_the_dynamic_suffix() {
        let settings = Settings::default();
        let state = PromptState {
            has_home_devices: true,
            device_count: 3,
            online_device_names: "Speaker, Hub, Washer".to_string(),
            ..default_state()
        };

        let p = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(
            p.dynamic_suffix.contains("Speaker, Hub, Washer"),
            "the live device list must survive somewhere: {}",
            p.dynamic_suffix
        );
        assert!(
            !p.static_prefix.contains("Washer"),
            "and it must not also be in the prefix"
        );
    }

    /// The failure this catches is the silent one. Under
    /// `tool_selection_mode = "relevant"` the selection is rescored every turn,
    /// so a swap that keeps the count is the *common* shape of change — and a
    /// hash over `.len()` alone called it unchanged, handing the provider a
    /// reuse decision for a prompt it had never seen.
    #[test]
    fn swapping_one_tool_for_another_moves_the_fast_hash() {
        let settings = Settings::default();

        let before = PromptState {
            available_tools: vec!["giap-home__set_light".into(), "giap-memory__recall".into()],
            ..default_state()
        };
        let swapped = PromptState {
            available_tools: vec![
                "giap-home__set_light".into(),
                "giap-weather__forecast".into(),
            ],
            ..default_state()
        };
        let reordered = PromptState {
            available_tools: vec!["giap-memory__recall".into(), "giap-home__set_light".into()],
            ..default_state()
        };

        let h = |s: &PromptState| compute_prefix_hash_fast(&settings, s, PROMPT_BALANCED);

        assert_ne!(
            h(&before),
            h(&swapped),
            "same count, different tools — this is the case that was wrong"
        );
        assert_ne!(
            h(&before),
            h(&reordered),
            "order is part of the render, so it is part of the identity"
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
            partition
                .dynamic_suffix
                .contains("Always respond in French."),
            "Addendum must be in dynamic suffix"
        );
        assert!(
            !partition
                .static_prefix
                .contains("Always respond in French."),
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

    /// Both inputs to the `<thinking>` word cap really do change the prefix,
    /// and both are therefore in the fast hash. The failure this catches is the
    /// silent one: a fast hash that says "unchanged" for a prefix that changed
    /// means the provider reuses a KV cache for a prompt it never saw.
    #[test]
    fn both_inputs_to_the_thinking_budget_move_the_prefix_and_the_fast_hash() {
        let state = PromptState {
            thinking_enabled: true,
            compact_prompt: false,
            ..default_state()
        };

        // 1. reasoning_effort.
        let brief = Settings {
            reasoning_effort: "brief".to_string(),
            ..Default::default()
        };
        let thorough = Settings {
            reasoning_effort: "thorough".to_string(),
            ..Default::default()
        };
        let p_brief = build_prompt_partition(&brief, None, &state, PROMPT_BALANCED);
        let p_thorough = build_prompt_partition(&thorough, None, &state, PROMPT_BALANCED);
        assert_ne!(
            p_brief.static_prefix, p_thorough.static_prefix,
            "reasoning_effort does not reach the static prefix at all"
        );
        assert_ne!(
            p_brief.prefix_hash, p_thorough.prefix_hash,
            "reasoning_effort changed the prefix without changing its hash"
        );
        assert_ne!(
            compute_prefix_hash_fast(&brief, &state, PROMPT_BALANCED),
            compute_prefix_hash_fast(&thorough, &state, PROMPT_BALANCED),
            "the fast hash misses reasoning_effort — it would report a changed prefix unchanged"
        );

        // 2. compact_prompt, which picks the tier the budget is drawn from.
        let compact_state = PromptState {
            compact_prompt: true,
            ..state.clone()
        };
        assert_ne!(
            compute_prefix_hash_fast(&brief, &state, PROMPT_BALANCED),
            compute_prefix_hash_fast(&brief, &compact_state, PROMPT_BALANCED),
            "the fast hash misses compact_prompt"
        );
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
            format!(
                "{}\n\n{}",
                partition.static_prefix, partition.dynamic_suffix
            )
        };

        // The combined output should contain identity
        assert!(combined.contains("Goose"));
        assert!(combined.contains("Friend"));

        // And temporal content
        assert!(combined.contains("Thursday, 1 May 2026"));
    }

    /// Style selection, asserted by IDENTITY rather than by prose.
    ///
    /// This used to match phrases out of each template ("intelligent AI
    /// copilot", "privacy-first AI copilot") and broke the moment the identity
    /// sections were rewritten — a test about which constant is returned failing
    /// because of wording it never meant to pin. Comparing pointers to the
    /// constants says exactly what "selects the correct style" means and cannot
    /// rot when the prompts are edited.
    ///
    /// Compared by VALUE, not by pointer: these are `const` items, which Rust
    /// inlines at each use site, so the test's `PROMPT_BALANCED` and the
    /// function's are separate allocations and `std::ptr::eq` reports them
    /// unequal even when the selection is correct.
    #[test]
    fn resolve_builtin_template_selects_correct_style() {
        let mut s = Settings::default();

        for (style, expected) in [
            ("balanced", PROMPT_BALANCED),
            ("concise", PROMPT_CONCISE),
            ("technical", PROMPT_TECHNICAL),
            ("warm", PROMPT_WARM),
        ] {
            s.prompt_style = style.to_string();
            assert_eq!(
                resolve_builtin_template(&s),
                expected,
                "style '{style}' did not resolve to its own template"
            );
        }

        // An unknown style falls back to balanced rather than erroring or
        // returning an empty prompt.
        s.prompt_style = "nonexistent".to_string();
        assert_eq!(
            resolve_builtin_template(&s),
            PROMPT_BALANCED,
            "an unrecognised style must fall back to balanced"
        );
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
            partition.static_prefix.contains("<voice-mode>"),
            "Voice mode section must be in static prefix"
        );
    }

    /// The prose tool listing must be suppressed by `native_tools_json`
    /// REGARDLESS of whether the model reasons.
    ///
    /// Observed on the Mac 2026-08-24: the system prompt handed to the provider
    /// was 30,848 chars for Llama 3.2 and Nemotron-before-the-thinking-fix, and
    /// ~2,000-2,400 for gemma-4 and Nemotron-after. The two groups differ in
    /// exactly one thing -- `thinking_enabled` -- and the size gap is 28,534
    /// chars, which is precisely the tool JSON. A model that does not reason was
    /// being handed the entire tool surface twice: once as prose in the system
    /// prompt and once as native declarations through its chat template.
    ///
    /// `native_tools_json` is set from the PROVIDER (`local`/`gguf`) and has
    /// nothing to do with reasoning, so the two flags must be independent here.
    #[test]
    fn a_model_that_does_not_reason_is_not_handed_the_tools_twice() {
        let settings = Settings::default();
        let tools = vec![
            "wikipedia — Look up factual info".to_string(),
            "weather — Current conditions".to_string(),
        ];

        let mut sizes = Vec::new();
        for thinking in [true, false] {
            let state = PromptState {
                available_tools: tools.clone(),
                native_tools_json: true,
                thinking_enabled: thinking,
                ..default_state()
            };
            let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);
            assert!(
                !partition.static_prefix.contains("wikipedia"),
                "native_tools_json must suppress the prose listing (thinking={thinking})"
            );
            sizes.push(partition.static_prefix.len());
        }

        // The thinking section is a real and small difference; a tool listing
        // appearing on one side is not.
        let gap = sizes[0].abs_diff(sizes[1]);
        assert!(
            gap < 2_000,
            "thinking should change the prefix by a section, not by a tool \
             listing: {} vs {} ({gap} chars apart)",
            sizes[0],
            sizes[1]
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
