//! Deterministic in-turn history trimmer — the hard-real-time half of hybrid
//! compaction.
//!
//! Runs before every agent turn on a neutral representation of the engine's
//! conversation. Never calls a model, never blocks: stale `<system-context>`
//! blocks are stripped from PRIOR user messages, oversized tool results are
//! truncated, the rolling `<conversation-summary>` (produced in idle time by
//! `SessionSummaryService`) is spliced at the front, and the oldest COMPLETE
//! turns are dropped until the estimate fits the profile's history budget.
//! Tool results always travel with their turn — the model never sees an
//! orphan tool result.
//!
//! Token counting comes from the injected [`TokenCounter`] port, so pond-core
//! still carries no tokenizer dependency of its own. Callers that can reach a
//! real tokenizer pass one; everything else passes
//! [`HeuristicTokenCounter`](super::token_counting::HeuristicTokenCounter),
//! which is the chars/4 arithmetic this module used to hardcode.
//!
//! The feedback correction stays regardless of which counter is used, and it is
//! load-bearing: no counter available here is *exact* (see the port's docs on
//! why tiktoken against a Gemma GGUF is not), and an image contributes only its
//! surrounding text to the estimate rather than the ~250 tokens it really
//! costs. When the previous turn's REAL prompt count (from `TurnStats`)
//! overshot the usable ceiling, the effective budget shrinks by that overshoot,
//! which converges in one turn.
//!
//! # Images are deliberately NOT handled here
//!
//! [`TrimMessage`] is text-only, and the image cap runs in the adapter AFTER
//! [`trim_history`] returns, over the messages that survived: capping an image
//! on a turn that is about to be dropped is wasted work, and the policy itself
//! belongs to `super::image_history`, shared with the hydration replay. Do not
//! add a second image rule inside this module — the two would drift, and the
//! adapter's is the one the engine actually sees.
//!
//! `MAX_HISTORY_REPLAY_IMAGES` is what bounds the resulting under-count: at one
//! replayed image the estimate is short by roughly 250 tokens, not by a
//! multiple of it.

use std::borrow::Cow;
use std::time::Duration;

use super::context_budget::{truncate_head_tail, CompactionProfile, TOOL_RESULT_MAX_CHARS};
use super::token_counting::PER_MESSAGE_TOKEN_OVERHEAD;
use crate::models::ports::token_counter::TokenCounter;

/// Floor for the history budget after the overshoot correction. Below roughly
/// this, a turn carries no usable context at all, and dropping to zero would
/// make the assistant forget the message it is answering.
const MIN_HISTORY_TOKENS: usize = 64;

/// Days of history the trimmer treats as *verbatim* before age weighting is
/// allowed to degrade it. The default for `Settings::compaction_verbatim_days`
/// reads this constant, so the setting's default and the code's cannot drift.
///
/// Three days is deliberately generous. The damaging direction is *short*: a
/// horizon inside the span of a normal conversation would hard-truncate tool
/// results the model is still reasoning about, and the saving is a few hundred
/// tokens. A horizon that is too long only means the household pays what it
/// pays today.
pub const DEFAULT_VERBATIM_DAYS: u32 = 3;

/// Tool-result cap applied to material older than the verbatim horizon —
/// a quarter of [`TOOL_RESULT_MAX_CHARS`].
///
/// This is the whole of PAI-4 P3's escalation rung: a three-day-old tool result
/// is not worth the same tokens as one from five minutes ago, but it is still
/// worth more than nothing, which is what dropping its turn would leave.
pub const AGED_TOOL_RESULT_MAX_CHARS: usize = TOOL_RESULT_MAX_CHARS / 4;

/// Convert a stored `compaction_verbatim_days` into the horizon
/// [`trim_history`] takes. Zero means age weighting is OFF, and that is the
/// only way to disable it — there is no separate boolean to fall out of step
/// with the number.
pub fn verbatim_horizon_from_days(days: u32) -> Option<Duration> {
    if days == 0 {
        return None;
    }
    Some(Duration::from_secs(u64::from(days) * 24 * 60 * 60))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimRole {
    User,
    Assistant,
    ToolResult,
}

/// One conversation message in engine-neutral form. `index` keys back into
/// the source conversation so adapters can rebuild engine messages without
/// this module knowing their shape.
#[derive(Debug, Clone)]
pub struct TrimMessage {
    pub index: usize,
    pub role: TrimRole,
    pub text: String,
    /// True for the spliced `<conversation-summary>` message.
    pub is_summary: bool,
    /// How old this message is, in seconds, measured by the CALLER at trim
    /// time. `None` means the caller could not establish an age.
    ///
    /// `None` is treated as *recent* everywhere, which is the narrowing
    /// direction: an unknown age never earns a message extra degradation. Only
    /// the live Goose turn path populates this today (from
    /// `goose::conversation::message::Message::created`); the hydration replay
    /// does not, and says so.
    pub age_secs: Option<u64>,
}

#[derive(Debug)]
pub struct TrimOutcome {
    pub messages: Vec<TrimMessage>,
    pub dropped_turns: usize,
    pub estimated_tokens: usize,
    /// Tool results re-truncated at [`AGED_TOOL_RESULT_MAX_CHARS`] because they
    /// fell outside the verbatim horizon. Zero whenever age weighting is off,
    /// no message carried an age, or the conversation already fit.
    pub aged_truncations: usize,
    /// False when the input already fit and nothing was modified — the
    /// adapter can skip rewriting the engine conversation entirely.
    pub changed: bool,
}

/// Whether the current turn's user message is already in the slice handed to
/// [`trim_history`].
///
/// This is the only thing that decides which user messages carry a *stale*
/// `<system-context>`, and it was previously assumed rather than passed — the
/// trimmer always spared the last user message on the grounds that it was the
/// current turn's fresh injection. In production it never is: the Goose adapter
/// calls `trim_goose_history` *before* `Agent::reply` appends anything, so the
/// last user message present is the PREVIOUS turn's, and sparing it re-prefilled
/// a stale block — wrong date, the previous message's selected memories, the
/// previous turn-budget note — on the one turn where it was about to matter, and
/// handed the model two conflicting `<system-context>` blocks.
///
/// It also made `TrimOutcome::changed` true on every turn from the third
/// onwards, which defeated the adapter's `if !changed { return; }` and forced a
/// whole-table rewrite of the engine conversation every single turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentTurn {
    /// The caller trims before appending this turn. Every user message present
    /// is history, so every one of them is stale. This is the production shape.
    NotYetAppended,
    /// The caller trims after appending. The last user message is this turn's
    /// fresh injection and must survive.
    AlreadyAppended,
}

/// Remove a `<system-context>…</system-context>` block from a prior user
/// message. Those blocks carry per-turn date/time and memory injections that
/// are stale in history — the current turn re-injects fresh ones.
pub fn strip_system_context(text: &str) -> Cow<'_, str> {
    const OPEN: &str = "<system-context>";
    const CLOSE: &str = "</system-context>";
    let Some(start) = text.find(OPEN) else {
        return Cow::Borrowed(text);
    };
    let Some(close) = text[start..].find(CLOSE) else {
        return Cow::Borrowed(text);
    };
    let end = start + close + CLOSE.len();
    let mut out = String::with_capacity(text.len() - (end - start));
    out.push_str(&text[..start]);
    out.push_str(text[end..].trim_start_matches('\n'));
    Cow::Owned(out)
}

fn estimate_tokens(messages: &[TrimMessage], counter: &dyn TokenCounter) -> usize {
    messages
        .iter()
        .map(|m| counter.count(&m.text) + PER_MESSAGE_TOKEN_OVERHEAD)
        .sum()
}

/// Index of the first message of the LAST complete turn (a turn = a user
/// message plus everything after it until the next user message).
fn last_turn_start(messages: &[TrimMessage]) -> usize {
    messages
        .iter()
        .rposition(|m| m.role == TrimRole::User && !m.is_summary)
        .unwrap_or(0)
}

/// Deterministically trim `messages` to fit within the profile's history
/// budget. `rolling_summary`, when present, is spliced (or refreshed) as a
/// summary message at the front. `last_real_prompt_tokens` is the previous
/// turn's engine-reported prompt size, used to tighten `counter`'s estimate.
///
/// `verbatim_horizon` is PAI-4 P3's age weighting: material older than it may
/// be degraded harder than fresh material *once the conversation is already
/// over budget*. `None` disables it and reproduces the pre-P3 behaviour
/// exactly.
pub fn trim_history(
    messages: Vec<TrimMessage>,
    profile: &CompactionProfile,
    rolling_summary: Option<&str>,
    last_real_prompt_tokens: Option<u32>,
    counter: &dyn TokenCounter,
    current_turn: CurrentTurn,
    verbatim_horizon: Option<Duration>,
) -> TrimOutcome {
    let mut changed = false;

    // Effective budget.
    //
    // Two corrections to the declared history budget, both aimed at the same
    // failure: a prompt that fills the window leaves the model no room to
    // answer, the engine raises ContextLengthExceeded mid-generation, and goose
    // reacts by compacting the conversation out from under us (a path that
    // ignores GOOSE_AUTO_COMPACT_THRESHOLD, so it cannot be turned off).
    //
    // 1. Never promise more history than fits alongside the output reserve AND
    //    the preamble. This clamp used to subtract only the reserve, which was
    //    never sufficient and PAI-3 P4 said so before deferring the fix to P5:
    //    at an 8,192 window the profile declares 1,024 + 3,000 + 500 + 4,000 =
    //    8,524 tokens, and a reserve-only clamp let history claim 7,168 of them
    //    on top of a preamble that was still going to be sent. The preamble
    //    allowance is now subtracted here, via `usable_history_tokens`, which on
    //    a local provider is the CLAMPED allowance — see
    //    `CompactionProfile::for_windows`. Floored at MIN_HISTORY_TOKENS so a
    //    preamble wider than the window squeezes history rather than erasing the
    //    message being answered.
    // 2. When the engine's real prompt count for the LAST turn overshot the
    //    usable ceiling, subtract that overshoot. This is measured, not
    //    estimated, so it corrects the counter's approximation in the direction
    //    that matters and converges within one turn. The comparison stays
    //    against `usable_prompt_tokens` — the engine reports the WHOLE prompt,
    //    preamble included, so the history-only ceiling would report an
    //    overshoot on every turn that merely used its budget.
    let usable = profile.usable_prompt_tokens();
    let mut budget = if usable > 0 {
        profile
            .history_token_budget
            .min(profile.usable_history_tokens())
            .max(MIN_HISTORY_TOKENS)
    } else {
        profile.history_token_budget
    };
    if let Some(real) = last_real_prompt_tokens {
        let real = real as usize;
        if usable > 0 && real > usable {
            budget = budget.saturating_sub(real - usable).max(MIN_HISTORY_TOKENS);
            changed = true;
        }
    }

    // 1. Strip stale <system-context> from every PRIOR user message. Which ones
    //    those are is the caller's to say (see `CurrentTurn`) — the adapter trims
    //    before the turn is appended, so on that path there is no message to
    //    spare and `spare_last_user` is None.
    let mut msgs: Vec<TrimMessage> = messages;
    let spare_last_user = match current_turn {
        CurrentTurn::AlreadyAppended => msgs
            .iter()
            .rposition(|m| m.role == TrimRole::User && !m.is_summary),
        CurrentTurn::NotYetAppended => None,
    };
    for (i, m) in msgs.iter_mut().enumerate() {
        if m.role == TrimRole::User && !m.is_summary && Some(i) != spare_last_user {
            if let Cow::Owned(stripped) = strip_system_context(&m.text) {
                m.text = stripped;
                changed = true;
            }
        }
    }

    // 2. Truncate oversized tool results, head+tail. The adapter applies the
    //    SAME helper to the structured tool response it rebuilds, so this
    //    estimate matches what the model actually receives — for a long time it
    //    did not, and a 50K-char result was re-prefilled verbatim every turn
    //    while the estimate believed it was 1.5K.
    for m in msgs.iter_mut() {
        if m.role == TrimRole::ToolResult {
            if let Some(truncated) = truncate_head_tail(&m.text, TOOL_RESULT_MAX_CHARS) {
                m.text = truncated;
                changed = true;
            }
        }
    }

    // 3. Splice or refresh the rolling summary at the front.
    if let Some(summary) = rolling_summary {
        let body = format!("<conversation-summary>\n{summary}\n</conversation-summary>");
        match msgs.iter_mut().find(|m| m.is_summary) {
            Some(existing) => {
                if existing.text != body {
                    existing.text = body;
                    changed = true;
                }
            }
            None => {
                msgs.insert(
                    0,
                    TrimMessage {
                        index: usize::MAX,
                        role: TrimRole::User,
                        text: body,
                        is_summary: true,
                        // The summary is generated now, whatever it summarises.
                        age_secs: Some(0),
                    },
                );
                changed = true;
            }
        }
    }

    // 4. Age-weighted degradation (PAI-4 P3). One rung, inserted between "leave
    //    it alone" and "drop the whole turn": a tool result older than the
    //    verbatim horizon is re-truncated at AGED_TOOL_RESULT_MAX_CHARS, which
    //    is a quarter of the flat cap step 2 already applied.
    //
    //    THREE GUARDS, and each is the narrowing direction on a different axis.
    //
    //    a. It fires only when the conversation is ALREADY over budget.
    //       Invariant 4 forbids perturbing a warm prefix for a marginal token
    //       saving, and this edit is at the FRONT of the conversation — it
    //       invalidates the KV prefix exactly as dropping a turn would. Doing
    //       it unconditionally would spend a full re-prefill to save tokens
    //       nobody needed. Over budget, the prefix was moving anyway, and the
    //       only question left is what gets sacrificed.
    //    b. The LAST turn is spared. That is the design's "current session,
    //       recent turns: verbatim" row made real, and it matters most in the
    //       case age weighting is aimed at: a session reopened after a week has
    //       every message past the horizon, including the one the model is
    //       about to answer.
    //    c. `age_secs: None` is never aged. A caller that cannot establish an
    //       age gets today's behaviour rather than a guess.
    //
    //    What this rung deliberately does NOT do is drop aged turns outright,
    //    which is what 3.2's table ("older than compaction_verbatim_days:
    //    represented by the rolling summary only") asks for. The trimmer cannot
    //    honour that claim: the summary's coverage is a through-pointer into
    //    `session_messages` (`SessionSummaryService`), the trimmer sees the
    //    ENGINE conversation with positional indices, and there is no mapping
    //    between them here. Worse, the summariser always leaves the newest
    //    KEEP_RECENT_MESSAGES = 6 uncovered by construction, so "the summary
    //    represents it" is provably false for the tail. Dropping on that
    //    premise would lose messages nothing had recorded, silently. Degrading
    //    them instead keeps the head and tail of every aged result and loses
    //    nothing that was not already being lost when the turn was dropped.
    let mut aged_truncations = 0usize;
    if let Some(horizon) = verbatim_horizon {
        if estimate_tokens(&msgs, counter) > budget {
            let horizon_secs = horizon.as_secs();
            let keep_verbatim_from = last_turn_start(&msgs);
            for (i, m) in msgs.iter_mut().enumerate() {
                if i >= keep_verbatim_from
                    || m.role != TrimRole::ToolResult
                    || !m.age_secs.is_some_and(|age| age > horizon_secs)
                {
                    continue;
                }
                if let Some(truncated) = truncate_head_tail(&m.text, AGED_TOOL_RESULT_MAX_CHARS) {
                    m.text = truncated;
                    aged_truncations += 1;
                    changed = true;
                }
            }
        }
    }

    // 5. Drop oldest complete turns (never the summary, never the last turn)
    //    until within budget. Age needs no say in the ORDER here and is given
    //    none: age is monotonic with position in a conversation, so
    //    "oldest-first" already is "most-aged-first", and re-deriving it from
    //    timestamps would be a second implementation of the same ordering with
    //    a clock skew failure mode the current one cannot have.
    let mut dropped_turns = 0usize;
    loop {
        let estimated = estimate_tokens(&msgs, counter);
        if estimated <= budget {
            break;
        }
        let keep_from = last_turn_start(&msgs);
        // First droppable message: skip the summary if present.
        let first_real = msgs.iter().position(|m| !m.is_summary).unwrap_or(0);
        if first_real >= keep_from {
            break; // only the last turn (+ summary) remains — nothing left to drop
        }
        // Drop one complete turn: from first_real through the end of that turn.
        let turn_end = msgs
            .iter()
            .enumerate()
            .skip(first_real + 1)
            .find(|(_, m)| m.role == TrimRole::User && !m.is_summary)
            .map(|(i, _)| i)
            .unwrap_or(keep_from);
        msgs.drain(first_real..turn_end);
        dropped_turns += 1;
        changed = true;
    }

    let estimated_tokens = estimate_tokens(&msgs, counter);
    TrimOutcome {
        messages: msgs,
        dropped_turns,
        estimated_tokens,
        aged_truncations,
        changed,
    }
}

/// Shape a conversation read back from durable storage for replay into a FRESH
/// engine session, budgeted exactly like a live conversation would have been.
///
/// This is the hydration half of history durability: engines keep their own
/// conversation store, so a restart (or a wiped engine store) can leave an
/// existing chat pointing at an empty engine session while the durable store
/// still holds every message. [`trim_history`] alone cannot cover it — it runs
/// against the engine's conversation and returns early when that is empty.
///
/// Two rules beyond plain trimming:
///
/// 1. Empty-text messages are dropped: providers reject empty turns.
/// 2. TRAILING user messages are dropped. The caller persists the incoming user
///    message BEFORE starting the turn, so the tail of durable history is the
///    very message the engine is about to append — replaying it would duplicate
///    it. Dropping the trailing run also leaves the replay ending on an
///    assistant turn, the correct shape to append a user message to.
///
/// Tool results are the caller's problem: a durable store generally cannot
/// reconstruct a provider-valid tool request/response pair, and an orphaned tool
/// response breaks the provider, so callers should filter them out before
/// calling this.
pub fn plan_replay(
    messages: Vec<(TrimRole, String)>,
    profile: &CompactionProfile,
    rolling_summary: Option<&str>,
    counter: &dyn TokenCounter,
) -> Vec<TrimMessage> {
    let mut rows: Vec<(TrimRole, String)> = messages
        .into_iter()
        .filter(|(_, text)| !text.trim().is_empty())
        .collect();
    while matches!(rows.last(), Some((TrimRole::User, _))) {
        rows.pop();
    }
    if rows.is_empty() {
        return Vec::new();
    }

    let trim_input: Vec<TrimMessage> = rows
        .into_iter()
        .enumerate()
        .map(|(index, (role, text))| TrimMessage {
            index,
            role,
            text,
            is_summary: false,
            // Age weighting is NOT wired on the hydration path, deliberately.
            // This function's input is `(role, text)` pairs; the durable rows do
            // carry timestamps, but threading them here widens the tuple through
            // the adapter's own pre-filtering (which assigns the indices this
            // function relies on) to buy a rung that only fires when a replay is
            // over budget — at which point the drop loop already handles it.
            // PAI-4 P3 leaves this `None` rather than inventing an age.
            age_secs: None,
        })
        .collect();
    // Trailing user messages were popped above, so nothing here is the current
    // turn — every `<system-context>` in this input is stale by construction.
    trim_history(
        trim_input,
        profile,
        rolling_summary,
        None,
        counter,
        CurrentTurn::NotYetAppended,
        None,
    )
    .messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::services::context::token_counting::HeuristicTokenCounter;

    fn profile(history_budget: usize) -> CompactionProfile {
        CompactionProfile {
            compaction_threshold: 0.6,
            memory_token_budget: 200,
            max_memory_fragments: 3,
            system_prompt_budget: 1500,
            history_token_budget: history_budget,
            // Zero here so the existing cases keep exercising exactly the
            // budget they pass in; the reserve has its own tests below.
            output_reserve_tokens: 0,
            context_window_tokens: 3072,
            prompt_window_tokens: 3072,
        }
    }

    /// Same shape, but with a reserve — for the ceiling/overshoot cases.
    fn profile_reserved(history_budget: usize, ctx: usize, reserve: usize) -> CompactionProfile {
        CompactionProfile {
            compaction_threshold: 0.6,
            memory_token_budget: 200,
            max_memory_fragments: 3,
            system_prompt_budget: 1500,
            history_token_budget: history_budget,
            output_reserve_tokens: reserve,
            context_window_tokens: ctx,
            prompt_window_tokens: ctx,
        }
    }

    /// The Orin case. A flat 1,200-token history budget against a 4,096 window
    /// promised more than the window could hold once the ~3,250-token preamble
    /// and the model's own output are accounted for. The budget must never
    /// exceed context minus the output reserve.
    #[test]
    fn history_budget_never_exceeds_the_window_minus_the_output_reserve() {
        let p = profile_reserved(1200, 4096, 768);
        assert_eq!(p.usable_prompt_tokens(), 3328);

        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();
        let out = trim_history(
            msgs,
            &p,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        // 1200 <= 3328, so the declared budget still applies here.
        assert!(out.estimated_tokens <= 1200, "got {}", out.estimated_tokens);
    }

    /// A tiny window where the reserve is the binding constraint: the declared
    /// budget is larger than what is left after reserving output room, so the
    /// smaller of the two must win.
    #[test]
    fn the_reserve_wins_when_it_is_tighter_than_the_declared_budget() {
        let p = profile_reserved(4000, 2048, 768); // usable = 1280
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();
        let out = trim_history(
            msgs,
            &p,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(
            out.estimated_tokens <= 1280,
            "history must fit the usable window, got {}",
            out.estimated_tokens
        );
    }

    /// PAI-3 P5. The declared history budget is capped by what is left once the
    /// preamble has been paid for, not merely by what is left once the output
    /// reserve has been. Without this, a real 8,192-token profile lets history
    /// claim 7,168 tokens on top of a 3,500-token preamble it is also going to
    /// send — 10,668 against an 8,192-token window, which is the mid-generation
    /// overrun `output_reserve_tokens` exists to prevent, arriving by the other
    /// door.
    #[test]
    fn the_history_clamp_subtracts_the_preamble_not_just_the_output_reserve() {
        // The real 8,192 profile, not a hand-built fixture: the point is that
        // the numbers the system actually ships are over-committed.
        let p = CompactionProfile::from_context_window(8_192);
        assert_eq!(p.history_token_budget, 4_000);
        assert_eq!(p.usable_prompt_tokens(), 7_168);
        assert_eq!(p.usable_history_tokens(), 3_668);

        // ~2,000 tokens of history offered; the clamp must cut it to 3,668, and
        // more to the point must never let it reach 7,168.
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(1_000))).collect();
        let out = trim_history(
            msgs,
            &p,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(
            out.estimated_tokens <= 3_668,
            "history claimed {} tokens, past the {} the preamble leaves it",
            out.estimated_tokens,
            p.usable_history_tokens()
        );
        assert!(
            out.dropped_turns > 0,
            "nothing was dropped, so the clamp never bound"
        );
        // And the whole prompt now fits the window it was budgeted for.
        assert!(
            out.estimated_tokens
                + p.system_prompt_budget
                + p.memory_token_budget
                + p.output_reserve_tokens
                <= p.context_window_tokens,
            "budgeted prompt still overruns the window"
        );
    }

    /// The other half: a preamble allowance wider than the window must squeeze
    /// history to the floor, never below it, because the message being answered
    /// still has to travel.
    #[test]
    fn a_preamble_wider_than_the_window_squeezes_history_to_the_floor() {
        // usable = 1,280; preamble allowance 1,700 -> usable_history saturates
        // to 0, and the floor takes over.
        let p = profile_reserved(4_000, 2_048, 768);
        assert_eq!(p.usable_history_tokens(), 0);
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();
        let out = trim_history(
            msgs,
            &p,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(out.estimated_tokens > 0, "must keep the current turn");
        assert_eq!(out.messages.len(), 1, "only the current turn survives");
    }

    /// Measured feedback: when the engine reports a real prompt that overshot
    /// the usable ceiling, the next turn's budget drops by exactly that
    /// overshoot — this is what stops the runaway that ended conversations.
    #[test]
    fn a_real_prompt_over_the_ceiling_shrinks_the_next_budget_by_the_overshoot() {
        let p = profile_reserved(1200, 4096, 768); // usable = 3328
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();

        let baseline = trim_history(
            msgs.clone(),
            &p,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        )
        .estimated_tokens;
        // Engine said the last prompt was 3,786 tokens — 458 over the ceiling.
        let corrected = trim_history(
            msgs,
            &p,
            None,
            Some(3786),
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        )
        .estimated_tokens;
        assert!(
            corrected < baseline,
            "overshoot must tighten the budget: {corrected} vs {baseline}"
        );
        assert!(corrected <= 1200 - 458 + 40, "got {corrected}");
    }

    /// The floor: a catastrophic overshoot must still leave enough room to
    /// carry the turn being answered, not collapse to nothing.
    #[test]
    fn the_budget_never_collapses_below_the_floor() {
        let p = profile_reserved(1200, 4096, 768);
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();
        let out = trim_history(
            msgs,
            &p,
            None,
            Some(100_000),
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(out.estimated_tokens > 0, "must keep the current turn");
    }

    fn user(index: usize, text: &str) -> TrimMessage {
        TrimMessage {
            index,
            role: TrimRole::User,
            text: text.to_string(),
            is_summary: false,
            age_secs: None,
        }
    }
    fn assistant(index: usize, text: &str) -> TrimMessage {
        TrimMessage {
            index,
            role: TrimRole::Assistant,
            text: text.to_string(),
            is_summary: false,
            age_secs: None,
        }
    }
    fn tool(index: usize, text: &str) -> TrimMessage {
        TrimMessage {
            index,
            role: TrimRole::ToolResult,
            text: text.to_string(),
            is_summary: false,
            age_secs: None,
        }
    }

    /// Same as [`tool`], but carrying an age — the only builder that can put a
    /// message outside the verbatim horizon.
    fn aged_tool(index: usize, text: &str, age_secs: u64) -> TrimMessage {
        TrimMessage {
            age_secs: Some(age_secs),
            ..tool(index, text)
        }
    }

    /// Three days, the default horizon.
    fn horizon() -> Option<Duration> {
        verbatim_horizon_from_days(DEFAULT_VERBATIM_DAYS)
    }

    const DAY: u64 = 24 * 60 * 60;

    #[test]
    /// The production shape: the adapter trims before the turn is appended, so
    /// EVERY user message present is history and every block in it is stale.
    ///
    /// This test used to assert the opposite — that `msgs[2]` kept its block —
    /// with a fixture whose last message was the current turn's. Nothing in
    /// production ever built that input: `trim_goose_history` runs before
    /// `Agent::reply`. It was a green test for a state that could not occur,
    /// which is the unreachable-fixture trap CLAUDE.md warns about.
    #[test]
    fn every_user_message_is_stale_when_the_turn_has_not_been_appended_yet() {
        let wrapped =
            "<system-context>\nToday is X\n</system-context>\n<user-message>hi</user-message>";
        let msgs = vec![user(0, wrapped), assistant(1, "hello"), user(2, wrapped)];
        let out = trim_history(
            msgs,
            &profile(10_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(out.changed);
        for i in [0, 2] {
            assert!(
                !out.messages[i].text.contains("<system-context>"),
                "message {i} kept a stale system-context block"
            );
            assert!(out.messages[i]
                .text
                .contains("<user-message>hi</user-message>"));
        }
    }

    /// The other half of the contract, so the enum cannot quietly become a
    /// one-armed switch.
    #[test]
    fn the_appended_current_turn_keeps_its_fresh_injection() {
        let wrapped =
            "<system-context>\nToday is X\n</system-context>\n<user-message>hi</user-message>";
        let msgs = vec![user(0, wrapped), assistant(1, "hello"), user(2, wrapped)];
        let out = trim_history(
            msgs,
            &profile(10_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::AlreadyAppended,
            None,
        );
        assert!(out.changed);
        assert!(!out.messages[0].text.contains("<system-context>"));
        assert!(out.messages[2].text.contains("<system-context>"));
    }

    /// The write-amplification guard. A conversation that already fits and has
    /// no stale blocks must report `changed == false`, or the adapter's early
    /// return never fires and it rewrites goose's whole message table per turn.
    #[test]
    fn a_steady_state_conversation_reports_no_change() {
        let msgs = vec![
            user(0, "<user-message>hi</user-message>"),
            assistant(1, "hello"),
            user(2, "<user-message>again</user-message>"),
            assistant(3, "sure"),
        ];
        let out = trim_history(
            msgs,
            &profile(10_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(
            !out.changed,
            "nothing was stale or oversized, yet the adapter was told to rewrite"
        );
    }

    #[test]
    fn truncates_oversized_tool_results() {
        let big = format!("START{}END", "x".repeat(TOOL_RESULT_MAX_CHARS + 500));
        let msgs = vec![user(0, "check"), tool(1, &big), assistant(2, "done")];
        let out = trim_history(
            msgs,
            &profile(10_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(out.changed);
        assert!(out.messages[1].text.len() < TOOL_RESULT_MAX_CHARS + 64);
        // Head+tail: the conclusion at the end of a tool result survives.
        assert!(out.messages[1].text.starts_with("START"));
        assert!(out.messages[1].text.ends_with("END"));
        assert!(out.messages[1].text.contains("[... truncated "));
    }

    #[test]
    fn splices_summary_at_front_and_refreshes_it() {
        let msgs = vec![user(0, "a"), assistant(1, "b")];
        let out = trim_history(
            msgs,
            &profile(10_000),
            Some("we discussed ducks"),
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(out.messages[0].is_summary);
        assert!(out.messages[0].text.contains("<conversation-summary>"));
        assert!(out.messages[0].text.contains("we discussed ducks"));

        // Refresh replaces the body, does not duplicate.
        let out2 = trim_history(
            out.messages,
            &profile(10_000),
            Some("now geese"),
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        let summaries: Vec<_> = out2.messages.iter().filter(|m| m.is_summary).collect();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].text.contains("now geese"));
    }

    #[test]
    fn drops_oldest_complete_turns_never_orphaning_tool_results() {
        // Three turns; tiny budget forces dropping the oldest two.
        let filler = "w".repeat(400);
        let msgs = vec![
            user(0, &filler),
            tool(1, &filler),
            assistant(2, &filler),
            user(3, &filler),
            assistant(4, &filler),
            user(5, "latest question"),
            assistant(6, "latest answer"),
        ];
        let out = trim_history(
            msgs,
            &profile(100),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert_eq!(out.dropped_turns, 2);
        // Whole turns went together: no leading tool/assistant orphans.
        assert_eq!(out.messages.first().unwrap().role, TrimRole::User);
        assert!(out
            .messages
            .first()
            .unwrap()
            .text
            .contains("latest question"));
    }

    #[test]
    fn last_turn_survives_even_over_budget() {
        let huge = "y".repeat(4_000);
        let msgs = vec![user(0, &huge), assistant(1, &huge)];
        let out = trim_history(
            msgs,
            &profile(50),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert_eq!(out.messages.len(), 2, "the current turn is never dropped");
    }

    #[test]
    fn trimming_is_idempotent() {
        let filler = "z".repeat(400);
        let msgs = vec![
            user(0, &filler),
            assistant(1, &filler),
            user(2, "q"),
            assistant(3, "a"),
        ];
        let once = trim_history(
            msgs,
            &profile(100),
            Some("sum"),
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        let twice = trim_history(
            once.messages.clone(),
            &profile(100),
            Some("sum"),
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(!twice.changed, "second pass must be a no-op");
        assert_eq!(once.messages.len(), twice.messages.len());
    }

    #[test]
    fn unchanged_input_reports_changed_false() {
        let msgs = vec![user(0, "hi"), assistant(1, "hello")];
        let out = trim_history(
            msgs,
            &profile(10_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(!out.changed);
        assert_eq!(out.dropped_turns, 0);
    }

    #[test]
    fn real_token_feedback_tightens_budget() {
        // Real prompt (6144) was double the window (3072) → budget halves →
        // the same history that fit at 200 no longer fits.
        let filler = "v".repeat(400);
        let msgs = vec![
            user(0, &filler),
            assistant(1, &filler),
            user(2, "q"),
            assistant(3, "a"),
        ];
        let relaxed = trim_history(
            msgs.clone(),
            &profile(300),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert_eq!(relaxed.dropped_turns, 0);
        let tightened = trim_history(
            msgs,
            &profile(300),
            None,
            Some(6144),
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(tightened.dropped_turns > 0);
    }

    // ── plan_replay (C1 hydration) ───────────────────────────────────────

    fn rows(pairs: &[(TrimRole, &str)]) -> Vec<(TrimRole, String)> {
        pairs.iter().map(|(r, t)| (*r, (*t).to_string())).collect()
    }

    /// The duplication bug this guards: the caller persists the incoming user
    /// message before the turn starts, so the tail of durable history IS the
    /// message the engine is about to append.
    #[test]
    fn replay_drops_the_trailing_user_message() {
        let out = plan_replay(
            rows(&[
                (TrimRole::User, "first"),
                (TrimRole::Assistant, "answer"),
                (TrimRole::User, "the message about to be sent"),
            ]),
            &profile(10_000),
            None,
            &HeuristicTokenCounter,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].role, TrimRole::Assistant);
        assert!(!out.iter().any(|m| m.text.contains("about to be sent")));
    }

    /// A conversation that is nothing but pending user messages replays as
    /// nothing at all — hydrating it would only duplicate the current turn.
    #[test]
    fn replay_of_only_user_messages_is_empty() {
        assert!(plan_replay(
            rows(&[(TrimRole::User, "hello")]),
            &profile(10_000),
            None,
            &HeuristicTokenCounter
        )
        .is_empty());
        assert!(plan_replay(vec![], &profile(10_000), None, &HeuristicTokenCounter).is_empty());
    }

    #[test]
    fn replay_skips_blank_messages() {
        let out = plan_replay(
            rows(&[
                (TrimRole::User, "q"),
                (TrimRole::Assistant, "   "),
                (TrimRole::Assistant, "real"),
            ]),
            &profile(10_000),
            None,
            &HeuristicTokenCounter,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].text, "real");
    }

    #[test]
    fn replay_splices_the_rolling_summary_and_respects_the_budget() {
        let filler = "z".repeat(2_000);
        let out = plan_replay(
            rows(&[
                (TrimRole::User, "oldest"),
                (TrimRole::Assistant, &filler),
                (TrimRole::User, "newer"),
                (TrimRole::Assistant, "kept"),
            ]),
            &profile(200),
            Some("earlier: the user set up two lamps"),
            &HeuristicTokenCounter,
        );
        assert!(out[0].is_summary);
        assert!(out[0].text.contains("<conversation-summary>"));
        // The oversized oldest turn was dropped to fit the budget.
        assert!(!out.iter().any(|m| m.text == filler));
        assert!(out.iter().any(|m| m.text == "kept"));
    }

    /// Replay is stable: hydrating a session twice yields the same plan.
    #[test]
    fn replay_is_idempotent() {
        let input = rows(&[
            (TrimRole::User, "q1"),
            (TrimRole::Assistant, "a1"),
            (TrimRole::User, "q2"),
            (TrimRole::Assistant, "a2"),
        ]);
        let first = plan_replay(
            input.clone(),
            &profile(10_000),
            Some("s"),
            &HeuristicTokenCounter,
        );
        let again: Vec<(TrimRole, String)> = first
            .iter()
            .filter(|m| !m.is_summary)
            .map(|m| (m.role, m.text.clone()))
            .collect();
        let second = plan_replay(again, &profile(10_000), Some("s"), &HeuristicTokenCounter);
        assert_eq!(
            first.iter().map(|m| m.text.clone()).collect::<Vec<_>>(),
            second.iter().map(|m| m.text.clone()).collect::<Vec<_>>()
        );
    }

    // ── PAI-4 P3: age-weighted retention ────────────────────────────────────
    //
    // Budgets here are chosen so the rung is REACHABLE. It only fires when the
    // conversation is already over budget, so a test written against a generous
    // budget asserts `aged_truncations == 0` about a mechanism that was never
    // invited to run -- which is how the first draft of three of these passed
    // while proving nothing. Each test below either forces an overflow or
    // states, in the same test, that the aged case does fire at that budget.

    /// A conversation whose old tool results are the reason it overflows now
    /// survives with its turns intact: the aged results shrink to a quarter of
    /// the flat cap and the drop loop has less work, or none.
    ///
    /// THIS IS THE PHASE'S MAIN GUARD. Without the rung, the same input loses
    /// whole turns.
    #[test]
    fn an_aged_tool_result_is_degraded_before_its_turn_is_dropped() {
        let big = "y".repeat(4_000);
        let msgs = vec![
            user(0, "first question"),
            aged_tool(1, &big, 9 * DAY),
            assistant(2, "answer one"),
            user(3, "second question"),
            aged_tool(4, &big, 8 * DAY),
            assistant(5, "answer two"),
            user(6, "current question"),
        ];

        let without = trim_history(
            msgs.clone(),
            &profile(600),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        let with = trim_history(
            msgs,
            &profile(600),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );

        assert!(
            without.dropped_turns > 0,
            "fixture is not over budget -- the rung would never be invited to run"
        );
        assert_eq!(
            with.aged_truncations, 2,
            "both aged tool results should have been re-truncated"
        );
        assert!(
            with.dropped_turns < without.dropped_turns,
            "age weighting must save turns the flat trimmer dropped: with={} without={}",
            with.dropped_turns,
            without.dropped_turns
        );
        for m in with
            .messages
            .iter()
            .filter(|m| m.role == TrimRole::ToolResult)
        {
            assert!(
                m.text.len() <= AGED_TOOL_RESULT_MAX_CHARS + 64,
                "aged result was not re-truncated: {} chars",
                m.text.len()
            );
        }
    }

    /// Invariant 4. The SAME conversation that gets degraded at a tight budget
    /// must come through a generous one untouched by the rung, or age weighting
    /// is spending a full re-prefill to save tokens nobody needed.
    #[test]
    fn age_weighting_never_touches_a_conversation_that_already_fits() {
        let msgs = vec![
            user(0, "q"),
            aged_tool(1, &"y".repeat(4_000), 30 * DAY),
            assistant(2, "a"),
            user(3, "current"),
        ];
        let tight = trim_history(
            msgs.clone(),
            &profile(300),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );
        assert_eq!(
            tight.aged_truncations, 1,
            "control: this conversation IS degradable when over budget"
        );

        let roomy = trim_history(
            msgs,
            &profile(100_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );
        assert_eq!(roomy.aged_truncations, 0);
        assert_eq!(roomy.dropped_turns, 0);
        // The flat cap from step 2 still applies -- that is pre-P3 behaviour.
        // What must NOT have happened is the tighter aged cap.
        let tool_text = &roomy
            .messages
            .iter()
            .find(|m| m.role == TrimRole::ToolResult)
            .expect("tool result kept")
            .text;
        assert!(
            tool_text.len() > AGED_TOOL_RESULT_MAX_CHARS,
            "a fitting conversation was degraded anyway: {} chars",
            tool_text.len()
        );
    }

    /// The "current session, recent turns: verbatim" row. A session reopened
    /// after a week has EVERY message past the horizon, including the one the
    /// model is about to answer -- that one is still spared.
    #[test]
    fn the_last_turn_is_verbatim_even_when_the_whole_session_is_aged() {
        let big = "y".repeat(4_000);
        let out = trim_history(
            vec![
                user(0, "old question"),
                aged_tool(1, &big, 9 * DAY),
                assistant(2, "old answer"),
                user(3, "the question being answered"),
                aged_tool(4, &big, 9 * DAY),
            ],
            &profile(600),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );
        assert_eq!(
            out.aged_truncations, 1,
            "exactly the tool result OUTSIDE the last turn may be degraded"
        );
        let last = out.messages.last().expect("last turn survives");
        assert_eq!(last.role, TrimRole::ToolResult);
        assert!(
            last.text.len() > AGED_TOOL_RESULT_MAX_CHARS,
            "the last turn's tool result was degraded: {} chars",
            last.text.len()
        );
    }

    /// Both narrowing defaults, at a budget where the rung provably DOES fire
    /// for an aged message: an unknown age and a fresh age are treated
    /// identically to each other, and neither earns extra degradation.
    #[test]
    fn an_unknown_or_fresh_age_is_never_degraded() {
        let big = "y".repeat(4_000);
        let run = |age: Option<u64>| {
            trim_history(
                vec![
                    user(0, "q"),
                    TrimMessage {
                        age_secs: age,
                        ..tool(1, &big)
                    },
                    assistant(2, "a"),
                    user(3, "current"),
                ],
                &profile(300),
                None,
                None,
                &HeuristicTokenCounter,
                CurrentTurn::NotYetAppended,
                horizon(),
            )
        };
        assert_eq!(
            run(Some(9 * DAY)).aged_truncations,
            1,
            "control: at this budget an AGED message is degraded"
        );
        for age in [None, Some(0), Some(DAY), Some(3 * DAY)] {
            assert_eq!(
                run(age).aged_truncations,
                0,
                "age {age:?} is inside the horizon and must not be degraded"
            );
        }
    }

    /// Zero days is the off switch, and off must mean identical to pre-P3.
    #[test]
    fn zero_verbatim_days_disables_age_weighting_entirely() {
        assert_eq!(verbatim_horizon_from_days(0), None);
        assert_eq!(
            verbatim_horizon_from_days(DEFAULT_VERBATIM_DAYS),
            Some(Duration::from_secs(3 * DAY))
        );

        let msgs = vec![
            user(0, "q"),
            aged_tool(1, &"y".repeat(4_000), 30 * DAY),
            assistant(2, "a"),
            user(3, "current"),
        ];
        let run = |h: Option<Duration>| {
            trim_history(
                msgs.clone(),
                &profile(300),
                None,
                None,
                &HeuristicTokenCounter,
                CurrentTurn::NotYetAppended,
                h,
            )
        };
        let on = run(horizon());
        let off = run(verbatim_horizon_from_days(0));
        let pre_p3 = run(None);

        assert_eq!(
            on.aged_truncations, 1,
            "control: the horizon is doing something at this budget"
        );
        assert_eq!(off.aged_truncations, 0);
        assert_eq!(off.dropped_turns, pre_p3.dropped_turns);
        assert_eq!(
            off.messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            pre_p3
                .messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            "zero days must reproduce pre-P3 output exactly"
        );
        assert_ne!(
            on.messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            off.messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            "on and off must differ, or the off switch proves nothing"
        );
    }

    /// Tool results still travel with their turn (invariant 2) and the result
    /// is idempotent once the rung has fired: re-running the trimmer on its own
    /// output degrades nothing further.
    #[test]
    fn age_weighting_is_idempotent_and_orphans_nothing() {
        let big = "y".repeat(4_000);
        let msgs = vec![
            user(0, "q1"),
            aged_tool(1, &big, 9 * DAY),
            assistant(2, "a1"),
            user(3, "current"),
        ];
        let first = trim_history(
            msgs,
            &profile(300),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );
        assert_eq!(first.aged_truncations, 1);
        for (i, m) in first.messages.iter().enumerate() {
            if m.role == TrimRole::ToolResult {
                assert!(
                    first.messages[..i]
                        .iter()
                        .any(|p| p.role == TrimRole::User && !p.is_summary),
                    "orphaned tool result at {i}"
                );
            }
        }

        let second = trim_history(
            first.messages.clone(),
            &profile(300),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );
        assert_eq!(second.aged_truncations, 0, "second pass degraded again");
        assert_eq!(
            first
                .messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            second
                .messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>()
        );
    }
}
