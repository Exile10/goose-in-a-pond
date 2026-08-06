//! Large-tier re-summarisation -- PAI-4's model axis, as a pure gate.
//!
//! `docs/architecture/pai/04-smart-compaction.md` section 3.1 gives the large
//! tier one mechanism the other two do not get: "**LLM re-summarisation of the
//! summary itself** when it grows stale. Here the summarisation call is cheap
//! and off the critical path." This module is the arithmetic that decides when
//! that is true, and how many tokens the rebuilt summary may spend.
//!
//! It is a *pure* gate in the shape [`super::resume_compaction`] proved: plain
//! integers in, a `Run`/`Skip` out, every rule unit-testable without a clock, a
//! database or a model. The mechanism that acts on it lives with the only other
//! writer of `sessions.rolling_summary`,
//! `shared::services::session_summary::SessionSummaryService::resummarise` --
//! deliberately, because two files writing one column is how this programme has
//! produced drift before.
//!
//! # What "grows stale" actually means
//!
//! `SessionSummaryService::refresh` is *incremental*. Each pass feeds the model
//! the previous summary plus the messages it does not yet cover, and asks for
//! "3-5 sentences maximum". So the summary is a fixed-size artefact standing in
//! for an unbounded and growing span, rebuilt every time from its own previous
//! output rather than from the conversation. Detail lost on pass *n* cannot come
//! back on pass *n+1*: it is a lossy chain, and the loss compounds.
//!
//! On the small and medium tiers that is the right trade -- the alternative
//! costs a model call on the box the next turn's prefill needs. On the large
//! tier it is not: the call is somebody else's hardware, the window is generous,
//! and the source messages are still sitting in `session_messages`. So the large
//! tier's extra mechanism is to **rebuild the summary from the source span**
//! rather than from the chain, with a budget the window can afford.
//!
//! # The two conditions, and why neither is a magic number
//!
//! - **The span the summary covers has outgrown the history budget**
//!   ([`ResummariseGateInputs::covered_tokens`] >
//!   [`ResummariseGateInputs::history_token_budget`]). Below that the trimmer
//!   could still carry the covered messages verbatim, so the summary is a
//!   convenience; above it, at least part of that span reaches the model *only*
//!   through the summary, and its fidelity is load-bearing. The number is the
//!   trimmer's own budget, not a new one. This is a **sufficient** condition
//!   rather than a necessary one -- history is summary plus verbatim turns, so
//!   the real crossover comes slightly earlier -- and stating it conservatively
//!   is the narrowing direction: it declines to spend a model call in the
//!   uncertain region.
//! - **A rebuild has room to more than double what the summary spends**
//!   (`summary_tokens * `[`MIN_REBUILD_GAIN`]` <= budget`). The first condition
//!   alone is monotone -- PAI-4 P6's whole lesson is that a standing condition
//!   acted on without a limiter fires forever -- and this is what stops a
//!   summary that already spends its budget from being rewritten to the same
//!   size, over and over, for nothing.
//!
//! # What this does NOT claim, having checked
//!
//! It does not make the gate self-clearing, and the first draft of this module
//! said it did. `refresh` runs on every tier and asks for "3-5 sentences
//! maximum", so on the large tier the two mechanisms genuinely oscillate: as
//! soon as four new messages accumulate past the through-pointer, the next
//! refresh folds the rebuilt summary back down toward its own much smaller
//! budget, and the gate opens again. That is not a defect that hides -- each
//! firing restores fidelity the collapse destroyed, which is real work -- but it
//! does mean the rate is "at most one rebuild per compaction pass", not "once
//! per session".
//!
//! **The rate limiter is upstream and already exists**, which is why this module
//! does not grow one of its own. A compaction pass happens only when
//! `ContextMonitor::claim_compaction` grants it (once per
//! `COMPACTION_COOLDOWN_TURNS` recorded turns, PAI-4 P6) or when a session is
//! reopened past `resume_compaction_idle_secs` (P4). A second limiter here would
//! be a second copy of a rule those two phases already own.
//!
//! Reconciling the two budgets properly -- teaching `refresh` that a large
//! window can afford more than three sentences -- would change the summary on a
//! path P1 and P6 both argued about at length, and it needs the cache-age
//! measurement P5 is for. It is named here rather than done here.
//!
//! # Which direction is the dangerous one
//!
//! Running when we should not: a model call that competes with a turn. Not
//! running: the summary stays as good as it is today, which is the behaviour
//! every tier has had since hybrid compaction shipped. So every fallback here
//! resolves toward *not* running -- an unknown tier, an absent summary, a span
//! that does not fit, a rebuild with no headroom to gain all mean skip.

use super::context_budget::CompactionProfile;
use super::model_class::ModelClass;

/// The rebuilt summary may occupy at most this fraction of the history budget.
///
/// A sixteenth. The summary is a *header* on the history the trimmer splices,
/// not a replacement for it, and the value of the large tier is that it can keep
/// recent turns verbatim as well. At the large tier's floor (65,536 tokens,
/// `history_token_budget` 20,000) that is 1,250 tokens; at the 128,000 anchor
/// (80,000) it is 5,000, which [`RESUMMARY_MAX_BUDGET_TOKENS`] then caps.
pub const RESUMMARY_BUDGET_DIVISOR: usize = 16;

/// Floor for the rebuilt summary's budget.
///
/// Unreachable through the current anchors -- the large tier starts at a 1,250
/// token budget -- and kept because the anchors are a table somebody will edit.
/// A budget below this cannot hold more than the 3-5 sentences the incremental
/// summary already produces, so firing at all would spend a model call to
/// rewrite the same size artefact.
pub const RESUMMARY_MIN_BUDGET_TOKENS: usize = 256;

/// Ceiling for the rebuilt summary's budget.
///
/// Past roughly this the artefact stops being a summary and becomes an abridged
/// transcript that is re-prefilled on every single turn. 2,048 is four to eight
/// pages of prose -- far more than the 3-5 sentences it replaces, and still
/// under three percent of a 128K window.
pub const RESUMMARY_MAX_BUDGET_TOKENS: usize = 2_048;

/// Minimum multiple of the current summary's size that the budget must allow
/// before a rebuild is worth a model call.
///
/// Two: do not spend a model call for less than a doubling. Read the module
/// header before raising it -- this bounds the *pointless* rebuild, and it is
/// deliberately not doing the job of a rate limiter, which lives upstream.
pub const MIN_REBUILD_GAIN: usize = 2;

/// Why a re-summarisation did not run.
///
/// Ordered as the gate evaluates them, cheapest and most-invariant first, so a
/// caller logging the reason gets the *first* thing that was wrong rather than
/// an arbitrary one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// This model class may not spend a model call on reshaping history --
    /// PAI-4 invariant 3. The only class that may is [`ModelClass::Large`].
    TierForbidsModelCall,
    /// There is no rolling summary to rebuild yet. The incremental refresh
    /// produces the input to this mechanism; it does not replace it.
    NoSummaryYet,
    /// The covered span still fits the history budget, so the trimmer could
    /// carry it verbatim and the summary is not yet the only record of it.
    SpanStillFitsHistory,
    /// The budget does not allow a [`MIN_REBUILD_GAIN`]-fold improvement on the
    /// summary that is already stored.
    NoRoomToImprove,
    /// The through-pointer names a message that is not in the session, so there
    /// is no span to re-read. Degrades to "leave the summary alone".
    NothingCovered,
    /// Not even the newest covered message fits the prompt budget. Pathological
    /// -- one message larger than the whole window -- and the narrow answer is
    /// to leave the existing summary in place.
    SourceTooLargeToRead,
}

impl SkipReason {
    /// Stable log label, in the shape `resume_compaction::SkipReason::as_str`
    /// uses.
    pub fn as_str(&self) -> &'static str {
        match self {
            SkipReason::TierForbidsModelCall => "tier_forbids_model_call",
            SkipReason::NoSummaryYet => "no_summary_yet",
            SkipReason::SpanStillFitsHistory => "span_still_fits_history",
            SkipReason::NoRoomToImprove => "no_room_to_improve",
            SkipReason::NothingCovered => "nothing_covered",
            SkipReason::SourceTooLargeToRead => "source_too_large_to_read",
        }
    }
}

/// Everything the gate needs. All of it is measurable from the session row, the
/// stored messages and the profile the governor already resolved.
#[derive(Debug, Clone, Copy)]
pub struct ResummariseGateInputs {
    /// The active model's class, from
    /// [`ModelClass::from_resolution`](super::model_class::ModelClass::from_resolution).
    pub class: ModelClass,
    /// Tokens the stored rolling summary occupies. Zero means there is none.
    pub summary_tokens: usize,
    /// Tokens the messages the summary covers occupy, counted with the same
    /// counter as `summary_tokens` so the comparison is robust to that
    /// counter's bias.
    pub covered_tokens: usize,
    /// `CompactionProfile::history_token_budget` for the resolved window.
    pub history_token_budget: usize,
}

/// What the gate decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Rebuild, spending at most `budget_tokens` on the new summary.
    Run {
        budget_tokens: usize,
    },
    Skip(SkipReason),
}

/// Tokens the rebuilt summary may occupy, from the history budget of the
/// resolved window. See [`RESUMMARY_BUDGET_DIVISOR`].
pub fn resummary_budget_tokens(history_token_budget: usize) -> usize {
    (history_token_budget / RESUMMARY_BUDGET_DIVISOR)
        .clamp(RESUMMARY_MIN_BUDGET_TOKENS, RESUMMARY_MAX_BUDGET_TOKENS)
}

/// Tokens of *source* the rebuild prompt may carry.
///
/// The whole prompt budget of the window, minus the room the answer needs.
/// `usable_prompt_tokens` already subtracts `output_reserve_tokens`, but that
/// reserve is sized for a chat answer rather than for a summary of up to
/// [`RESUMMARY_MAX_BUDGET_TOKENS`], so the summary budget is subtracted again
/// rather than assumed to be covered. Subtracting twice is the narrow direction.
pub fn source_budget_tokens(profile: &CompactionProfile, summary_budget_tokens: usize) -> usize {
    profile
        .usable_prompt_tokens()
        .saturating_sub(summary_budget_tokens)
}

/// Whether the large tier should rebuild this session's rolling summary from
/// source, and what it may spend doing it.
pub fn should_resummarise(inputs: ResummariseGateInputs) -> GateDecision {
    if !inputs.class.permits_compaction_model_call() {
        return GateDecision::Skip(SkipReason::TierForbidsModelCall);
    }
    if inputs.summary_tokens == 0 {
        return GateDecision::Skip(SkipReason::NoSummaryYet);
    }
    if inputs.covered_tokens <= inputs.history_token_budget {
        return GateDecision::Skip(SkipReason::SpanStillFitsHistory);
    }
    let budget_tokens = resummary_budget_tokens(inputs.history_token_budget);
    if inputs.summary_tokens.saturating_mul(MIN_REBUILD_GAIN) > budget_tokens {
        return GateDecision::Skip(SkipReason::NoRoomToImprove);
    }
    GateDecision::Run { budget_tokens }
}

/// Index of the oldest covered message the rebuild prompt can afford, taking
/// the NEWEST affordable suffix of `per_message_tokens`.
///
/// Returns `0` when the whole span fits, and `per_message_tokens.len()` when not
/// even the last message does -- which the caller treats as
/// [`SkipReason::SourceTooLargeToRead`] rather than sending an empty span.
///
/// # Why the newest end
///
/// When the covered span does not fit, something has to be represented by the
/// old summary instead of by its source, and the design's own time axis (3.2)
/// says which end to give up: "a turn from three days ago is not worth the same
/// tokens as one from five minutes ago". Keeping the newest suffix is also what
/// P3's age-weighted retention will do, so the two cannot disagree later.
///
/// This is a bounded improvement rather than a full rebuild, and the doc comment
/// says so rather than letting the name imply otherwise. The full-rebuild case
/// is the common one: at the large tier's floor the source budget is over 63,000
/// tokens.
pub fn newest_affordable_start(per_message_tokens: &[usize], source_budget: usize) -> usize {
    let mut used = 0usize;
    let mut start = per_message_tokens.len();
    for (i, tokens) in per_message_tokens.iter().enumerate().rev() {
        if used + tokens > source_budget {
            break;
        }
        used += tokens;
        start = i;
    }
    start
}

/// The number of words to ask the model for, from a token budget.
///
/// Models take a word count far more reliably than a token count, and roughly
/// four tokens cover three English words. Rounded down, which keeps the ask
/// inside the budget rather than at it.
pub fn budget_as_words(budget_tokens: usize) -> usize {
    budget_tokens * 3 / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn large_inputs() -> ResummariseGateInputs {
        ResummariseGateInputs {
            class: ModelClass::Large,
            summary_tokens: 100,
            covered_tokens: 40_000,
            history_token_budget: 20_000,
        }
    }

    /// The invariant this gate exists to hold, and the first thing it checks.
    /// On either on-device tier a re-summarisation call competes with the
    /// prefill of the turn the user is waiting on (PAI-4 invariant 3).
    #[test]
    fn neither_on_device_tier_can_ever_reach_a_run() {
        for class in [ModelClass::Small, ModelClass::Medium] {
            // Everything else about these inputs screams "rebuild me".
            let inputs = ResummariseGateInputs {
                class,
                ..large_inputs()
            };
            assert_eq!(
                should_resummarise(inputs),
                GateDecision::Skip(SkipReason::TierForbidsModelCall),
                "the {} tier was cleared to spend a model call on re-summarisation",
                class.label()
            );
        }
        assert!(matches!(
            should_resummarise(large_inputs()),
            GateDecision::Run { .. }
        ));
    }

    /// The same property from the other direction: no combination of window,
    /// summary size and span size lets an on-device provider through. The
    /// classification comes from `ModelClass::classify`, so this exercises the
    /// composition rather than a hand-picked `ModelClass` literal.
    #[test]
    fn no_on_device_provider_reaches_a_run_at_any_window() {
        for provider in ["local", "gguf", "ollama", "llamafile"] {
            for window in [4_096usize, 12_288, 32_768, 65_536, 131_072, 1_000_000] {
                let class = ModelClass::classify(provider, window);
                let profile = CompactionProfile::from_context_window(window);
                let decision = should_resummarise(ResummariseGateInputs {
                    class,
                    summary_tokens: 1,
                    covered_tokens: usize::MAX,
                    history_token_budget: profile.history_token_budget,
                });
                assert_eq!(
                    decision,
                    GateDecision::Skip(SkipReason::TierForbidsModelCall),
                    "{provider} at {window} tokens was cleared for a re-summarisation call"
                );
            }
        }
    }

    /// No summary means nothing to rebuild. The incremental refresh produces
    /// this mechanism's input; it is not replaced by it.
    #[test]
    fn an_absent_summary_is_not_a_rebuild_opportunity() {
        let inputs = ResummariseGateInputs {
            summary_tokens: 0,
            ..large_inputs()
        };
        assert_eq!(
            should_resummarise(inputs),
            GateDecision::Skip(SkipReason::NoSummaryYet)
        );
    }

    /// While the covered span still fits the history budget the trimmer can
    /// carry it verbatim, so the summary is not the only record of it and a
    /// model call buys fidelity nobody is missing.
    #[test]
    fn a_span_that_still_fits_the_history_budget_is_left_alone() {
        for covered in [0usize, 1, 19_999, 20_000] {
            let inputs = ResummariseGateInputs {
                covered_tokens: covered,
                ..large_inputs()
            };
            assert_eq!(
                should_resummarise(inputs),
                GateDecision::Skip(SkipReason::SpanStillFitsHistory),
                "a {covered}-token span under a 20,000-token history budget triggered a rebuild"
            );
        }
        let inputs = ResummariseGateInputs {
            covered_tokens: 20_001,
            ..large_inputs()
        };
        assert!(matches!(
            should_resummarise(inputs),
            GateDecision::Run { .. }
        ));
    }

    /// A summary that already spends its budget is not rewritten to the same
    /// size for nothing. The boundary is exactly a doubling, and it is pinned
    /// from both sides because the whole point of `MIN_REBUILD_GAIN` is where it
    /// sits: this test's first draft asserted that HALF the budget closes the
    /// gate, which is off by one step and would have quietly described a
    /// stricter rule than the code implements.
    #[test]
    fn a_summary_with_nothing_to_gain_is_not_rebuilt() {
        let budget = resummary_budget_tokens(20_000);
        // Exactly a doubling is still worth the call.
        assert_eq!(
            should_resummarise(ResummariseGateInputs {
                summary_tokens: budget / MIN_REBUILD_GAIN,
                ..large_inputs()
            }),
            GateDecision::Run {
                budget_tokens: budget
            }
        );
        // One token more, and the best a rebuild could do is under a doubling.
        for summary_tokens in [
            budget / MIN_REBUILD_GAIN + 1,
            budget - 1,
            budget,
            budget * 4,
        ] {
            assert_eq!(
                should_resummarise(ResummariseGateInputs {
                    summary_tokens,
                    ..large_inputs()
                }),
                GateDecision::Skip(SkipReason::NoRoomToImprove),
                "a {summary_tokens}-token summary against a {budget}-token budget was \
                 sent back for a rebuild that cannot improve it"
            );
        }
    }

    /// The budget is a fraction of the trimmer's history budget, clamped at both
    /// ends, and both real large-tier anchors are pinned so a change to
    /// `PROFILE_ANCHORS` shows up here rather than as a silently bigger prompt.
    #[test]
    fn the_summary_budget_is_bounded_at_both_ends() {
        assert_eq!(resummary_budget_tokens(0), RESUMMARY_MIN_BUDGET_TOKENS);
        assert_eq!(resummary_budget_tokens(100), RESUMMARY_MIN_BUDGET_TOKENS);
        assert_eq!(
            resummary_budget_tokens(usize::MAX),
            RESUMMARY_MAX_BUDGET_TOKENS
        );
        // The large tier's floor: 65,536 tokens -> 20,000 history -> 1,250.
        let floor = CompactionProfile::from_context_window(65_536);
        assert_eq!(floor.history_token_budget, 20_000);
        assert_eq!(resummary_budget_tokens(floor.history_token_budget), 1_250);
        // The top anchor: 80,000 history would give 5,000, so the cap binds.
        let top = CompactionProfile::from_context_window(128_000);
        assert_eq!(top.history_token_budget, 80_000);
        assert_eq!(
            resummary_budget_tokens(top.history_token_budget),
            RESUMMARY_MAX_BUDGET_TOKENS
        );
    }

    /// The rebuilt summary must never be allowed to claim the space the verbatim
    /// turns need. A header, not a replacement.
    #[test]
    fn the_summary_budget_is_a_small_share_of_the_history_it_heads() {
        for window in [65_536usize, 96_000, 128_000, 200_000] {
            let profile = CompactionProfile::from_context_window(window);
            let budget = resummary_budget_tokens(profile.history_token_budget);
            assert!(
                budget * 8 <= profile.history_token_budget,
                "at a {window}-token window the rebuilt summary could claim {budget} of \
                 {} history tokens",
                profile.history_token_budget
            );
        }
    }

    /// The common case at the large tier is a FULL rebuild: the source budget
    /// comfortably exceeds the span that made the gate fire in the first place.
    #[test]
    fn the_large_tier_can_afford_to_re_read_the_whole_covered_span() {
        let profile = CompactionProfile::from_context_window(65_536);
        let budget = resummary_budget_tokens(profile.history_token_budget);
        let source = source_budget_tokens(&profile, budget);
        assert!(
            source > profile.history_token_budget,
            "the source budget ({source}) cannot even cover the history budget ({}), so a \
             rebuild would never re-read more than the trimmer already carries",
            profile.history_token_budget
        );
    }

    /// Everything fits: start at the beginning, which is the full-rebuild path.
    #[test]
    fn a_span_that_fits_is_read_from_the_beginning() {
        assert_eq!(newest_affordable_start(&[10, 10, 10], 30), 0);
        assert_eq!(newest_affordable_start(&[10, 10, 10], 1_000), 0);
        assert_eq!(newest_affordable_start(&[], 1_000), 0);
    }

    /// When it does not fit, the NEWEST suffix survives -- the design's time
    /// axis, and what P3's age weighting will do.
    #[test]
    fn an_oversized_span_keeps_its_newest_end() {
        assert_eq!(newest_affordable_start(&[10, 10, 10], 25), 1);
        assert_eq!(newest_affordable_start(&[10, 10, 10], 20), 1);
        assert_eq!(newest_affordable_start(&[10, 10, 10], 19), 2);
        assert_eq!(newest_affordable_start(&[10, 10, 10], 10), 2);
    }

    /// A single message bigger than the whole prompt budget yields an empty
    /// span, which the caller must treat as "leave the summary alone" rather
    /// than as "summarise nothing".
    #[test]
    fn a_span_whose_newest_message_does_not_fit_yields_no_span_at_all() {
        let counts = [10usize, 10, 10];
        assert_eq!(newest_affordable_start(&counts, 9), counts.len());
        assert_eq!(newest_affordable_start(&counts, 0), counts.len());
    }

    /// A zero-token message must not make the walk stop early, and must not let
    /// it run past the front of the slice.
    #[test]
    fn zero_token_messages_do_not_confuse_the_walk() {
        assert_eq!(newest_affordable_start(&[0, 0, 0], 0), 0);
        assert_eq!(newest_affordable_start(&[100, 0, 0], 0), 1);
    }

    #[test]
    fn the_word_ask_stays_inside_the_token_budget() {
        assert_eq!(budget_as_words(1_250), 937);
        assert_eq!(budget_as_words(0), 0);
        for budget in [256usize, 1_000, 2_048] {
            assert!(budget_as_words(budget) <= budget);
        }
    }

    /// The way this phase could be dead without any other test noticing: if
    /// `Large` were unreachable through the rungs an OFF-TURN pass can supply,
    /// every test above would still pass and nothing would ever rebuild in
    /// production. That is the `ProfileScope::Owner` failure — a fixture
    /// production cannot produce — and it is the one worth pinning explicitly.
    ///
    /// `compaction_model_class` in `pond-api` has no engine report (that arrives
    /// on `TurnStats`, i.e. only during a turn) and no registry pin (that is the
    /// adapter's). So it composes exactly the rungs below, and both live routes
    /// into the tier are asserted: a catalog row, which PAI-3 P3a taught the
    /// catalog providers to write, and the capability window the adapter derives
    /// from the model name.
    #[test]
    fn the_large_tier_is_reachable_through_the_rungs_an_off_turn_pass_can_supply() {
        use crate::models::services::context::context_governor::{ContextGovernor, ContextInputs};

        // Rung 3: a catalog row. `capability_window` carries the
        // `ModelCapabilities` default, which is what an unrefreshed adapter
        // reports, so the catalog is doing the work.
        let via_catalog = ContextInputs {
            provider: "anthropic",
            model: "claude-sonnet",
            catalog_context_length: Some(200_000),
            capability_window: Some(4_096),
            ..Default::default()
        };
        assert_eq!(
            ModelClass::from_resolution(
                via_catalog.provider,
                &ContextGovernor::resolve(&via_catalog)
            ),
            ModelClass::Large,
            "a hosted model with a catalog row cannot reach the tier that rebuilds, so \
             re-summarisation is dead in production"
        );

        // Rung 5: no catalog row, the adapter's name-derived capability window.
        let via_capability = ContextInputs {
            provider: "openai",
            model: "gemma-4-27b",
            capability_window: Some(128_000),
            ..Default::default()
        };
        assert_eq!(
            ModelClass::from_resolution(
                via_capability.provider,
                &ContextGovernor::resolve(&via_capability)
            ),
            ModelClass::Large
        );

        // And the narrow direction, which must stay narrow: a hosted model
        // nobody recognises reports the 4,096 default and stays small rather
        // than being trusted with a model call.
        let unknown = ContextInputs {
            provider: "openai",
            model: "some-new-thing",
            capability_window: Some(4_096),
            ..Default::default()
        };
        assert_eq!(
            ModelClass::from_resolution(unknown.provider, &ContextGovernor::resolve(&unknown)),
            ModelClass::Small
        );

        // The on-device counterpart through the same rungs: PAI-3 P3a's clamp
        // holds, so an Ollama model declaring 131,072 is still not cleared.
        let on_device = ContextInputs {
            provider: "ollama",
            model: "gemma4:e2b",
            catalog_context_length: Some(131_072),
            capability_window: Some(128_000),
            ..Default::default()
        };
        assert_eq!(
            ModelClass::from_resolution(on_device.provider, &ContextGovernor::resolve(&on_device)),
            ModelClass::Medium,
            "an on-device provider reached the tier that spends a model call"
        );
    }

    #[test]
    fn every_skip_reason_has_a_distinct_label() {
        let labels = [
            SkipReason::TierForbidsModelCall.as_str(),
            SkipReason::NoSummaryYet.as_str(),
            SkipReason::SpanStillFitsHistory.as_str(),
            SkipReason::NoRoomToImprove.as_str(),
            SkipReason::NothingCovered.as_str(),
            SkipReason::SourceTooLargeToRead.as_str(),
        ];
        let mut sorted = labels.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len());
    }
}
