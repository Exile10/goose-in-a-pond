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

/// Upper bound on any single embedded signal. Embedding models truncate anyway.
const MAX_SIGNAL_CHARS: usize = 2000;

/// The texts that represent a session's topic, each scored INDEPENDENTLY.
///
/// Returns the question first, then the standing context, and they must never be
/// concatenated. A 17-character question ("Check the weather") embedded together
/// with a kilobyte of memories yields a vector dominated by the memories: the
/// question's own topic drops below the threshold, nothing clears the bar, and
/// the top-scorer rescue then hands the session whichever group the *memory
/// blob* happens to resemble. Observed live — "Check the weather" selected
/// `giap-schedule` and left `giap-weather` dormant, so the model had no weather
/// tool at all.
///
/// Scored separately and combined with `max`, a specific question wins on its own
/// merits, while a vague opener ("what about tomorrow?") still falls back to the
/// standing context — which is why the memories were included in the first place.
pub fn selection_signals(first_message: &str, memories: &str) -> Vec<String> {
    [first_message, memories]
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.len() <= MAX_SIGNAL_CHARS {
                return s.to_string();
            }
            // Truncate on a char boundary — the signal is user text.
            let cut = s
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|i| *i <= MAX_SIGNAL_CHARS)
                .last()
                .unwrap_or(0);
            s[..cut].to_string()
        })
        .collect()
}

/// Combine per-signal scores into one score per group by taking the best.
///
/// `max` and not a mean: the signals are alternative descriptions of what the
/// session is about, not parts of one description, so a strong match on either
/// is a strong match. Averaging would reintroduce the dilution this split exists
/// to remove.
pub fn merge_scores(per_signal: &[Vec<GroupScore>]) -> Vec<GroupScore> {
    let mut merged: Vec<GroupScore> = Vec::new();
    for scores in per_signal {
        for s in scores {
            match merged.iter_mut().find(|m| m.extension == s.extension) {
                Some(m) => m.score = m.score.max(s.score),
                None => merged.push(s.clone()),
            }
        }
    }
    merged
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

/// Remove every tool belonging to a group a `Guest` must never reach.
///
/// See [`crate::mcp::domain::tool_group::groups_denied_to_guests`] for the list
/// and why it is a denylist rather than an allowlist.
///
/// **This is a PROMPT-SURFACE control, not an execution gate, and the
/// difference matters.** It decides what the model can SEE, which is what stops
/// it choosing a withheld tool. It does not stop one running: `goose_agent.rs`'s
/// tool-call guard says so in terms — by the time it runs, "the tool either has
/// run or is about to, and nothing here can stop it" — because goose keeps every
/// extension loaded agent-wide and collects every `ToolRequest` regardless of
/// which schemas were published. What that guard does is refuse to SURFACE the
/// call and its result. A real execution gate needs an inspector registered with
/// goose's `ToolInspectionManager`, whose `add_inspector` is private: a fork
/// patch, not a local change.
///
/// So this is the layer that makes a withheld capability unreachable in
/// practice, and it is defence in depth rather than a wall. It was previously
/// described here as "the enforcement point for PAI-1 P5", which reads as the
/// stronger claim and is worth not making, because the tool-narrowing design
/// leans on this function.
///
/// **It operates on TOOLS rather than groups deliberately.** The group-level
/// subtraction inside the
/// adapter's selection path only runs when
/// `settings.tool_selection_mode == "relevant"`, and the default is `"all"` --
/// so on a default install the selection path is skipped entirely and the
/// unfiltered tool set went straight to the model. A `Guest` turn kept
/// `giap-memory` and could recall, keyword-search or `forget_memory` the whole
/// household. P5 was recorded as landed while being inert on every default
/// pond.
///
/// The set published to the provider shim is the only thing that actually
/// constrains the model, so the check belongs here, where every mode converges.
/// Filtering by group before selection cannot work: `giap-memory` and
/// `giap-draft` are core groups and `select_groups` puts core groups back
/// unconditionally.
///
/// Idempotent, so applying it after a path that already subtracted at the group
/// level is harmless.
pub fn subtract_guest_denied_tools<'a, I>(tools: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a String>,
{
    let denied = crate::mcp::domain::tool_group::groups_denied_to_guests();
    tools
        .into_iter()
        .filter(|name| match group_of_tool(name) {
            // An unprefixed name belongs to no group and cannot be matched
            // against the denylist. Keeping it is the widening choice, but a
            // tool with no group is a platform tool, not personal data.
            Some(ext) => !denied.contains(&ext),
            None => true,
        })
        .cloned()
        .collect()
}

/// The groups a speaker with this scope may EVER hold.
///
/// This is the PAI-1 boundary as a value, and it exists because two callers used
/// to derive it independently and one of them got it wrong. `dormant_groups_note`
/// was built from the full registered list, so an unidentified speaker was shown
/// `giap-memory`, `giap-vision`, `giap-audit` and `giap-context` under a sentence
/// telling it that enabling one makes its tools available immediately — the
/// groups had been withheld from the selection and then advertised anyway. And
/// the escape hatch checked catalog membership and registration only, so the
/// speaker could take what the menu offered.
///
/// Applied to the candidates going INTO [`select_groups`] rather than subtracted
/// after. That works because `select_groups` filters the core set by `available`;
/// an older comment in the adapter claimed a pre-filter "would not stick because
/// select_groups puts core groups back unconditionally", which has not been true
/// for as long as that filter has existed.
///
/// A denylist, for the same reason `groups_denied_to_guests` is one: a new
/// extension is not personal data by default, and the failure mode of the
/// alternative is a capability silently missing rather than one silently granted.
pub fn permitted_groups(
    available: &[String],
    scope: &crate::user_data::domain::profile::ProfileScope,
) -> Vec<String> {
    if !scope.excludes_everything() {
        return available.to_vec();
    }
    let denied = crate::mcp::domain::tool_group::groups_denied_to_guests();
    available
        .iter()
        .filter(|e| !denied.contains(&e.as_str()))
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

    // ── The PAI-1 boundary ────────────────────────────────────────────────

    /// A guest may not hold a personal-data group, and may not be shown one.
    ///
    /// Both halves in one test because they were one bug. The groups were
    /// withheld from the selection and then advertised by
    /// `dormant_groups_note`, which was built from the full registered list —
    /// under a sentence that tells the model enabling a group makes its tools
    /// available immediately. Withholding a capability and then publishing a
    /// menu of it is worse than not withholding it, because it reads as an
    /// invitation.
    #[test]
    fn a_guest_is_neither_given_nor_offered_a_personal_group() {
        use crate::user_data::domain::profile::ProfileScope;

        let all = available();
        let denied = crate::mcp::domain::tool_group::groups_denied_to_guests();
        assert!(
            !denied.is_empty(),
            "the guest denylist is empty, so this test would pass against anything"
        );

        let permitted = permitted_groups(&all, &ProfileScope::Guest);

        // Held: nothing denied survives into the ceiling.
        for d in denied {
            assert!(
                !permitted.iter().any(|p| p == d),
                "'{d}' is denied to guests but is in the permitted set"
            );
        }
        assert!(
            permitted.len() < all.len(),
            "the guest ceiling is the whole catalog — the subtraction did nothing"
        );

        // Offered: the note may only name groups from the ceiling. Loaded is
        // empty, which is the worst case — everything permitted is dormant.
        let note = dormant_groups_note(&permitted, &[]);
        assert!(
            !note.is_empty(),
            "no note rendered, so the assertions below prove nothing"
        );
        for d in denied {
            assert!(
                !note.contains(d),
                "the dormant-groups note offers '{d}' to a guest:\n{note}"
            );
        }
    }

    /// An identified speaker loses nothing. The boundary is for guests only.
    #[test]
    fn an_identified_speaker_keeps_the_whole_catalog() {
        use crate::user_data::domain::profile::ProfileScope;

        let all = available();
        for scope in [
            ProfileScope::Household,
            ProfileScope::Owner("member-1".to_string()),
        ] {
            let permitted = permitted_groups(&all, &scope);
            assert_eq!(
                permitted.len(),
                all.len(),
                "{scope:?} lost groups it is entitled to"
            );
        }
    }

    /// The ceiling bounds selection, including the core groups.
    ///
    /// This is what lets the boundary be applied once, going in, rather than
    /// subtracted afterwards: `select_groups` filters `core_group_names()` by
    /// `available`, so passing the guest ceiling as `available` keeps `giap-draft`
    /// and `giap-memory` out even though both are core.
    #[test]
    fn selecting_from_the_guest_ceiling_drops_even_core_groups() {
        use crate::mcp::domain::tool_group::core_group_names;
        use crate::user_data::domain::profile::ProfileScope;

        let permitted = permitted_groups(&available(), &ProfileScope::Guest);
        let denied = crate::mcp::domain::tool_group::groups_denied_to_guests();

        // Precondition the whole approach rests on: at least one core group is
        // denied to guests. If that stopped being true this test would be
        // measuring nothing.
        let denied_core: Vec<&str> = core_group_names()
            .into_iter()
            .filter(|c| denied.contains(c))
            .collect();
        assert!(
            !denied_core.is_empty(),
            "no core group is denied to guests, so 'the pre-filter also bounds \
             core groups' is untested"
        );

        // No embedder — the widening path, which is what a fresh Jetson takes.
        let selection = select_groups(&permitted, None, DEFAULT_RELEVANCE_THRESHOLD);
        for c in &denied_core {
            assert!(
                !selection.groups.iter().any(|g| g == c),
                "core group '{c}' came back for a guest despite the ceiling"
            );
        }
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

    /// The regression this split exists for: the question and the standing
    /// context must reach the embedder as SEPARATE texts. Concatenated, a short
    /// question is drowned by a long memory block and its own topic drops below
    /// the threshold — live, "Check the weather" selected `giap-schedule` and
    /// left `giap-weather` dormant.
    #[test]
    fn question_and_memories_are_scored_separately() {
        let sigs = selection_signals("Check the weather", "- [identity] lives in Nairobi");
        assert_eq!(sigs.len(), 2);
        assert_eq!(sigs[0], "Check the weather", "the question stands alone");
        assert!(sigs[1].contains("Nairobi"));
        assert!(
            !sigs[0].contains("Nairobi"),
            "the memories must never be folded into the question"
        );
    }

    #[test]
    fn an_empty_half_is_dropped_not_embedded() {
        assert_eq!(selection_signals("hi", "   "), vec!["hi".to_string()]);
        assert_eq!(selection_signals("  ", "mems"), vec!["mems".to_string()]);
        assert!(selection_signals(" ", "").is_empty());
    }

    #[test]
    fn signals_are_bounded_and_utf8_safe() {
        let memories = "e\u{301}".repeat(4000); // multi-byte, well over the cap
        let sigs = selection_signals("hi", &memories);
        assert_eq!(sigs.len(), 2);
        assert!(sigs[1].len() <= 2001);
        // Truncation must not have split a char — the String is valid by
        // construction, so re-validating its chars is the assertion.
        assert!(sigs[1].chars().count() > 0);
    }

    /// Merging takes the best per group, not the mean — averaging would put the
    /// dilution back.
    #[test]
    fn merge_takes_the_best_score_per_group() {
        let a = vec![
            GroupScore {
                extension: "giap-weather".into(),
                score: 0.61,
            },
            GroupScore {
                extension: "giap-schedule".into(),
                score: 0.10,
            },
        ];
        let b = vec![
            GroupScore {
                extension: "giap-weather".into(),
                score: 0.05,
            },
            GroupScore {
                extension: "giap-schedule".into(),
                score: 0.31,
            },
        ];
        let merged = merge_scores(&[a, b]);
        let get = |e: &str| merged.iter().find(|m| m.extension == e).unwrap().score;
        assert!((get("giap-weather") - 0.61).abs() < 1e-6);
        assert!((get("giap-schedule") - 0.31).abs() < 1e-6);
    }

    /// End to end on the live failure: with the question scored on its own, the
    /// weather group clears the bar and is selected.
    #[test]
    fn a_specific_question_selects_its_group_despite_unrelated_memories() {
        let avail = available();
        // Question signal: strongly on-topic for weather. Memory signal: noise
        // about other things, mildly resembling schedule.
        let question = vec![
            GroupScore {
                extension: "giap-weather".into(),
                score: 0.62,
            },
            GroupScore {
                extension: "giap-schedule".into(),
                score: 0.08,
            },
        ];
        let mems = vec![
            GroupScore {
                extension: "giap-weather".into(),
                score: 0.04,
            },
            GroupScore {
                extension: "giap-schedule".into(),
                score: 0.17,
            },
        ];
        let merged = merge_scores(&[question, mems]);
        let sel = select_groups(&avail, Some(&merged), DEFAULT_RELEVANCE_THRESHOLD);
        assert!(
            sel.groups.contains(&"giap-weather".to_string()),
            "got {:?}",
            sel.groups
        );
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

    // ── PAI-1 P5: the guest denial, at the tool level ───────────────────────

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// The defect this function exists for. `tool_selection_mode` defaults to
    /// "all", which skips the whole selection path where the group-level
    /// subtraction lives -- so the unfiltered set reached the model and a Guest
    /// kept every memory tool. Nothing below depends on selection running.
    #[test]
    fn guest_denied_tools_are_removed_from_an_unfiltered_set() {
        let all = tools(&[
            "giap-memory__recall_memories",
            "giap-memory__forget_memory",
            "giap-memory__keyword_search",
            "giap-draft__approve_draft",
            "giap-audit__list_egress",
            "giap-vision__describe_scene",
            "giap-sensors__read_sensor",
            "giap-weather__get_forecast",
            "giap-toolkit__enable_tool_group",
        ]);

        let kept = subtract_guest_denied_tools(all.iter());

        for personal in [
            "giap-memory__recall_memories",
            "giap-memory__forget_memory",
            "giap-memory__keyword_search",
            "giap-draft__approve_draft",
            "giap-audit__list_egress",
            "giap-vision__describe_scene",
            "giap-sensors__read_sensor",
        ] {
            assert!(
                !kept.iter().any(|t| t == personal),
                "{personal} must not reach a guest"
            );
        }

        // A visitor keeps a useful assistant: the denial is targeted, not a
        // blanket refusal to work.
        assert!(kept.iter().any(|t| t == "giap-weather__get_forecast"));
        assert!(kept.iter().any(|t| t == "giap-toolkit__enable_tool_group"));
    }

    /// Assert the positive case too, not only the boundary. Three vacuous-test
    /// incidents in this programme were all an assertion that held trivially
    /// because the fixture was empty or wrong.
    #[test]
    fn a_non_guest_set_is_returned_intact() {
        let all = tools(&["giap-memory__recall_memories", "giap-weather__get_forecast"]);
        // The function is the guest branch; the caller decides when to apply it.
        // What it must never do is drop something that is not on the denylist.
        let kept = subtract_guest_denied_tools(all.iter());
        assert!(
            kept.iter().any(|t| t == "giap-weather__get_forecast"),
            "a non-personal tool was dropped: {kept:?}"
        );
        assert_eq!(kept.len(), 1, "exactly the one denied tool should go");
    }

    #[test]
    fn subtracting_twice_equals_subtracting_once() {
        let all = tools(&["giap-memory__recall_memories", "giap-weather__get_forecast"]);
        let once = subtract_guest_denied_tools(all.iter());
        let twice = subtract_guest_denied_tools(once.iter());
        assert_eq!(
            once, twice,
            "must be idempotent -- it runs after a path that may already have subtracted at the group level"
        );
    }

    /// An unprefixed tool has no group to match against the denylist. Keeping
    /// it is the widening choice, so it is stated explicitly rather than left
    /// to be discovered.
    #[test]
    fn an_ungrouped_tool_is_kept() {
        let all = tools(&["platform__final_output"]);
        assert_eq!(subtract_guest_denied_tools(all.iter()).len(), 1);
    }

    /// Every name in the denylist must actually be a group the catalog knows,
    /// or the subtraction silently protects nothing. A typo here is invisible:
    /// the filter would just never match.
    #[test]
    fn every_guest_denied_group_exists_in_the_catalog() {
        for g in crate::mcp::domain::tool_group::groups_denied_to_guests() {
            assert!(
                crate::mcp::domain::tool_group::is_catalog_extension(g),
                "{g} is on the guest denylist but is not a catalog extension -- \
                 the denial would match nothing"
            );
        }
    }
}
