//! Per-SESSION tool-relevance selection (Phase D2).
//!
//! Picks which `giap-*` extension groups have their tool SCHEMAS in the prompt
//! for a conversation. Pure functions — the embedding call and the persistence
//! live in the adapter.
//!
//! ## What this is not
//!
//! This is not a classifier and it does not decide whether the model uses tools.
//! GIAP's working agreement is "trust the model for tool use — no keyword
//! pre-classification", and this respects that: the model always receives a tool
//! surface and always decides natively via MCP whether and which tool to call.
//! What is decided here is only which schemas are physically present in the
//! prompt, for prompt COST — the same class of decision as the memory token
//! budget or the history trimmer, both of which already drop content to fit a
//! budget. And unlike a classifier, the decision is reversible BY THE MODEL:
//! `giap-toolkit`'s `enable_tool_group` pulls a dormant group in mid-session.
//!
//! ## Stickiness
//!
//! Selection runs once per session, not per turn. Per-turn churn would rewrite
//! the tools JSON on every turn and destroy the local engine's KV prefix reuse —
//! which is the entire reason to shrink the prompt in the first place.
//!
//! ## Bias
//!
//! Every failure path widens. No embedder, an embedding error, an empty
//! registered set, or a score that lands just under the line all resolve toward
//! MORE tools, never fewer. A missing tool costs the user a wrong answer; a
//! surplus tool costs ~100 tokens.

use crate::mcp::domain::tool_group::{
    core_group_names, find_group, group_of_tool, is_catalog_extension, TOOL_GROUPS,
};

/// Cosine-similarity floor for including a non-core group, on
/// all-MiniLM-L6-v2 (the fastembed model Phase A wired in).
///
/// Calibration on that model for short text: unrelated pairs land ~0.00-0.15,
/// loosely related ~0.20-0.35, clearly on-topic above ~0.40. 0.28 sits
/// deliberately BELOW the on-topic band — the asymmetry of costs (a surplus
/// group is ~100-1200 prompt tokens, a missing group is a wasted round trip at
/// ~10s on-device) means erring wide is correct.
pub const DEFAULT_RELEVANCE_THRESHOLD: f32 = 0.28;

/// Cosine similarity of the session's opening context against one group.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupScore {
    pub extension: String,
    pub score: f32,
}

/// How the selection was reached — surfaced in the trace line so the saving (or
/// the absence of one) is explainable after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionBasis {
    /// `tool_selection_mode = "all"`, or nothing to select from.
    ModeAll,
    /// No embedder available, or embedding/scoring failed — widened to all.
    NoEmbedder,
    /// Scored against group descriptions.
    Scored,
}

/// The outcome of selection for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSelection {
    /// Extension names whose tools go in the prompt. Sorted, deduped.
    pub groups: Vec<String>,
    pub basis: SelectionBasis,
}

impl ToolSelection {
    /// All registered groups — the "no narrowing" answer.
    pub fn all(available: &[String], basis: SelectionBasis) -> Self {
        let mut groups = available.to_vec();
        groups.sort();
        groups.dedup();
        Self { groups, basis }
    }
}

/// The text that gets embedded to represent a session's topic.
///
/// The first user message plus the memories injected on that turn. The memories
/// matter: they are what the assistant already knows about this user, so a
/// session opening with "what about tomorrow?" still scores against the standing
/// context rather than against nothing. Bounded because embedding models
/// truncate anyway and a huge memory block would drown the actual question.
pub fn selection_signal(first_message: &str, memories: &str) -> String {
    const MAX_SIGNAL_CHARS: usize = 2000;
    let mut signal = String::with_capacity(first_message.len() + memories.len() + 1);
    signal.push_str(first_message.trim());
    if !memories.trim().is_empty() {
        signal.push('\n');
        signal.push_str(memories.trim());
    }
    if signal.len() > MAX_SIGNAL_CHARS {
        // Truncate on a char boundary — the signal is user text.
        let cut = signal
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|i| *i <= MAX_SIGNAL_CHARS)
            .last()
            .unwrap_or(0);
        signal.truncate(cut);
    }
    signal
}

/// The group descriptions to embed, intersected with what is registered.
///
/// Core groups are excluded: they are never scored, so embedding them would be
/// wasted work.
pub fn scorable_groups(available: &[String]) -> Vec<(&'static str, &'static str)> {
    TOOL_GROUPS
        .iter()
        .filter(|g| !g.core)
        .filter(|g| available.iter().any(|a| a == g.extension))
        .map(|g| (g.extension, g.description))
        .collect()
}

/// Choose the groups for a session.
///
/// `available` — extensions actually registered this run (settings toggles
/// already applied). `scores` — cosine of the selection signal against each
/// scorable group, or `None` when no embedder was available or scoring failed.
///
/// Guarantees, in order of precedence:
/// 1. `scores == None` → every available group (never narrow blindly).
/// 2. Core groups are always present when registered.
/// 3. Every group at or above `threshold` is present.
/// 4. The single best-scoring non-core group is present even if it missed the
///    threshold — the cheapest possible insurance against a session that needs
///    one obvious capability the score merely under-rated.
pub fn select_groups(
    available: &[String],
    scores: Option<&[GroupScore]>,
    threshold: f32,
) -> ToolSelection {
    if available.is_empty() {
        return ToolSelection::all(available, SelectionBasis::ModeAll);
    }
    let Some(scores) = scores else {
        return ToolSelection::all(available, SelectionBasis::NoEmbedder);
    };

    let mut chosen: Vec<String> = core_group_names()
        .into_iter()
        .filter(|c| available.iter().any(|a| a == c))
        .map(|c| c.to_string())
        .collect();

    // Anything registered that the catalog does not describe is a user-added
    // external MCP extension: never narrowed, because the user added it on
    // purpose and there is no description to score it against.
    for ext in available {
        if !is_catalog_extension(ext) {
            chosen.push(ext.clone());
        }
    }

    for s in scores {
        if s.score >= threshold && available.iter().any(|a| a == &s.extension) {
            chosen.push(s.extension.clone());
        }
    }

    // Rule 4: rescue the top scorer regardless of the threshold.
    if let Some(best) = scores
        .iter()
        .filter(|s| available.iter().any(|a| a == &s.extension))
        .filter(|s| find_group(&s.extension).is_some_and(|g| !g.core))
        .max_by(|a, b| a.score.total_cmp(&b.score))
    {
        chosen.push(best.extension.clone());
    }

    chosen.sort();
    chosen.dedup();
    ToolSelection {
        groups: chosen,
        basis: SelectionBasis::Scored,
    }
}

/// Retain only the tools belonging to `groups`.
///
/// A tool with no `__` prefix, or one whose prefix is not a catalog extension,
/// is KEPT: the first is goose plumbing the shim's allow-set already governs,
/// the second is a user-added MCP server that selection does not own.
pub fn filter_tools_by_groups<'a, I>(tools: I, groups: &[String]) -> Vec<String>
where
    I: IntoIterator<Item = &'a String>,
{
    tools
        .into_iter()
        .filter(|name| match group_of_tool(name) {
            Some(ext) if is_catalog_extension(ext) => groups.iter().any(|g| g == ext),
            _ => true,
        })
        .cloned()
        .collect()
}

/// The `<tool-groups>` block for the user message's `<system-context>`.
///
/// Lists the groups that are NOT loaded, so the model can reach for
/// `enable_tool_group` without first spending a round trip on
/// `list_tool_groups`. Rides the user message, never the system prompt: it is
/// session-specific and the system prefix must stay byte-identical across
/// sessions for KV reuse.
///
/// Returns an empty string when nothing is dormant — no narrowing, no note.
pub fn dormant_groups_note(available: &[String], loaded: &[String]) -> String {
    let dormant: Vec<&'static crate::mcp::domain::tool_group::ToolGroup> = TOOL_GROUPS
        .iter()
        .filter(|g| available.iter().any(|a| a == g.extension))
        .filter(|g| !loaded.iter().any(|l| l == g.extension))
        .collect();
    if dormant.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(64 + dormant.len() * 80);
    out.push_str("<tool-groups>\n");
    out.push_str(
        "These extra tool groups exist but are not loaded right now. If you need one, call \
         enable_tool_group with its name and its tools become available immediately.\n",
    );
    for g in dormant {
        // First sentence only — enough to choose by, a fraction of the tokens.
        let gist = g
            .description
            .split_once(':')
            .map(|(head, _)| head)
            .unwrap_or(g.description);
        out.push_str("- ");
        out.push_str(g.extension);
        out.push_str(": ");
        out.push_str(gist.trim());
        out.push('\n');
    }
    out.push_str("</tool-groups>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available() -> Vec<String> {
        TOOL_GROUPS
            .iter()
            .map(|g| g.extension.to_string())
            .collect()
    }

    fn score(ext: &str, score: f32) -> GroupScore {
        GroupScore {
            extension: ext.to_string(),
            score,
        }
    }

    /// The fallback that makes narrowing safe: no embedder means no narrowing.
    #[test]
    fn no_scores_falls_back_to_every_group() {
        let avail = available();
        let sel = select_groups(&avail, None, DEFAULT_RELEVANCE_THRESHOLD);
        assert_eq!(sel.basis, SelectionBasis::NoEmbedder);
        assert_eq!(sel.groups.len(), avail.len());
        for ext in &avail {
            assert!(sel.groups.contains(ext), "{ext} missing from fallback");
        }
    }

    #[test]
    fn empty_availability_is_not_a_crash() {
        let sel = select_groups(&[], None, DEFAULT_RELEVANCE_THRESHOLD);
        assert!(sel.groups.is_empty());
        assert_eq!(sel.basis, SelectionBasis::ModeAll);
    }

    /// Core groups survive even when every score is zero.
    #[test]
    fn core_groups_are_always_present() {
        let avail = available();
        let scores: Vec<GroupScore> = scorable_groups(&avail)
            .iter()
            .map(|(e, _)| score(e, 0.0))
            .collect();
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        for c in core_group_names() {
            assert!(sel.groups.contains(&c.to_string()), "core {c} was dropped");
        }
    }

    /// The point of the feature: a weather question does not load 59 tools.
    #[test]
    fn a_single_relevant_group_narrows_hard() {
        let avail = available();
        let mut scores: Vec<GroupScore> = scorable_groups(&avail)
            .iter()
            .map(|(e, _)| score(e, 0.05))
            .collect();
        for s in scores.iter_mut() {
            if s.extension == "giap-weather" {
                s.score = 0.61;
            }
        }
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        assert_eq!(sel.basis, SelectionBasis::Scored);
        // 4 core + weather, and nothing else.
        assert_eq!(sel.groups.len(), core_group_names().len() + 1);
        assert!(sel.groups.contains(&"giap-weather".to_string()));
        assert!(!sel.groups.contains(&"giap-schedule".to_string()));
    }

    #[test]
    fn every_group_above_threshold_is_included() {
        let avail = available();
        let scores = vec![
            score("giap-device", 0.44),
            score("giap-device-control", 0.41),
            score("giap-schedule", 0.30),
            score("giap-news", 0.02),
        ];
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        for want in ["giap-device", "giap-device-control", "giap-schedule"] {
            assert!(sel.groups.contains(&want.to_string()), "{want} missing");
        }
        assert!(!sel.groups.contains(&"giap-news".to_string()));
    }

    /// Rule 4: the best non-core scorer rides along even below the threshold,
    /// so a merely under-rated capability does not cost a round trip.
    #[test]
    fn top_scorer_is_rescued_below_threshold() {
        let avail = available();
        let scores = vec![
            score("giap-finance", 0.19),
            score("giap-news", 0.11),
            score("giap-vision", 0.03),
        ];
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        assert!(
            sel.groups.contains(&"giap-finance".to_string()),
            "top scorer should be rescued"
        );
        assert!(!sel.groups.contains(&"giap-news".to_string()));
    }

    /// Selection only ever picks from what is registered — a disabled extension
    /// cannot be resurrected by a high score.
    #[test]
    fn unavailable_groups_are_never_selected() {
        let avail: Vec<String> = vec![
            "giap-draft".into(),
            "giap-toolkit".into(),
            "giap-weather".into(),
        ];
        let scores = vec![score("giap-schedule", 0.99), score("giap-weather", 0.50)];
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        assert!(!sel.groups.contains(&"giap-schedule".to_string()));
        assert!(sel.groups.contains(&"giap-weather".to_string()));
        // giap-memory / giap-system are core but not registered here.
        assert!(!sel.groups.contains(&"giap-memory".to_string()));
    }

    /// A user-added MCP server is not in the catalog, so selection leaves it be.
    #[test]
    fn external_extensions_are_never_narrowed() {
        let mut avail = available();
        avail.push("my-home-assistant-mcp".to_string());
        let scores: Vec<GroupScore> = scorable_groups(&avail)
            .iter()
            .map(|(e, _)| score(e, 0.0))
            .collect();
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        assert!(sel.groups.contains(&"my-home-assistant-mcp".to_string()));
    }

    #[test]
    fn scorable_groups_excludes_core_and_unregistered() {
        let avail: Vec<String> = vec![
            "giap-draft".into(),
            "giap-memory".into(),
            "giap-weather".into(),
        ];
        let scorable = scorable_groups(&avail);
        assert_eq!(scorable.len(), 1);
        assert_eq!(scorable[0].0, "giap-weather");
    }

    #[test]
    fn tool_filter_keeps_only_selected_groups() {
        let tools: Vec<String> = vec![
            "giap-weather__get_forecast".into(),
            "giap-weather__get_current_weather".into(),
            "giap-schedule__create_schedule".into(),
            "giap-memory__recall_memories".into(),
        ];
        let groups: Vec<String> = vec!["giap-weather".into(), "giap-memory".into()];
        let kept = filter_tools_by_groups(&tools, &groups);
        assert_eq!(kept.len(), 3);
        assert!(!kept.iter().any(|t| t.starts_with("giap-schedule")));
    }

    #[test]
    fn tool_filter_keeps_unknown_and_unprefixed_tools() {
        let tools: Vec<String> = vec![
            "my-mcp__do_thing".into(),
            "unprefixed_tool".into(),
            "giap-news__get_headlines".into(),
        ];
        let groups: Vec<String> = vec!["giap-weather".into()];
        let kept = filter_tools_by_groups(&tools, &groups);
        assert!(kept.contains(&"my-mcp__do_thing".to_string()));
        assert!(kept.contains(&"unprefixed_tool".to_string()));
        assert!(!kept.contains(&"giap-news__get_headlines".to_string()));
    }

    #[test]
    fn signal_combines_message_and_memories() {
        let s = selection_signal("what's the weather?", "- [identity] lives in Nairobi");
        assert!(s.contains("weather"));
        assert!(s.contains("Nairobi"));
    }

    #[test]
    fn signal_is_bounded_and_utf8_safe() {
        let memories = "e\u{301}".repeat(4000); // multi-byte, well over the cap
        let s = selection_signal("hi", &memories);
        assert!(s.len() <= 2001);
        // Truncation must not have split a char — the String is valid by
        // construction, so re-validating its chars is the assertion.
        assert!(s.chars().count() > 0);
    }

    #[test]
    fn dormant_note_lists_only_unloaded_groups() {
        let avail = available();
        let loaded: Vec<String> = core_group_names().iter().map(|c| c.to_string()).collect();
        let note = dormant_groups_note(&avail, &loaded);
        assert!(note.contains("<tool-groups>"));
        assert!(note.contains("giap-weather"));
        assert!(
            !note.contains("- giap-memory"),
            "loaded groups must not be advertised as dormant"
        );
        assert!(note.contains("enable_tool_group"));
    }

    /// With everything loaded there is nothing to advertise, so the note costs
    /// zero tokens — important for the default "all" mode.
    #[test]
    fn dormant_note_is_empty_when_nothing_is_dormant() {
        let avail = available();
        assert!(dormant_groups_note(&avail, &avail).is_empty());
    }
}
