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

use super::context_budget::{truncate_head_tail, CompactionProfile, TOOL_RESULT_MAX_CHARS};
use super::token_counting::PER_MESSAGE_TOKEN_OVERHEAD;
use crate::models::ports::token_counter::TokenCounter;

/// Floor for the history budget after the overshoot correction. Below roughly
/// this, a turn carries no usable context at all, and dropping to zero would
/// make the assistant forget the message it is answering.
const MIN_HISTORY_TOKENS: usize = 64;

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
}

#[derive(Debug)]
pub struct TrimOutcome {
    pub messages: Vec<TrimMessage>,
    pub dropped_turns: usize,
    pub estimated_tokens: usize,
    /// False when the input already fit and nothing was modified — the
    /// adapter can skip rewriting the engine conversation entirely.
    pub changed: bool,
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
pub fn trim_history(
    messages: Vec<TrimMessage>,
    profile: &CompactionProfile,
    rolling_summary: Option<&str>,
    last_real_prompt_tokens: Option<u32>,
    counter: &dyn TokenCounter,
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
    // 1. Never promise more history than fits alongside the output reserve.
    //    The tier constants are flat — 1,200 tokens at the 4K tier — and were
    //    blind to a preamble that measures ~3,250 tokens on the Orin, i.e. the
    //    declared budget alone already exceeded the window.
    // 2. When the engine's real prompt count for the LAST turn overshot the
    //    usable ceiling, subtract that overshoot. This is measured, not
    //    estimated, so it corrects the counter's approximation in the direction
    //    that matters and converges within one turn.
    let usable = profile.usable_prompt_tokens();
    let mut budget = if usable > 0 {
        profile.history_token_budget.min(usable)
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

    // 1. Strip stale <system-context> from all PRIOR user messages (every
    //    user message except the last one — the current turn's injection is
    //    added after trimming by the adapter).
    let mut msgs: Vec<TrimMessage> = messages;
    let last_user = msgs
        .iter()
        .rposition(|m| m.role == TrimRole::User && !m.is_summary);
    for (i, m) in msgs.iter_mut().enumerate() {
        if m.role == TrimRole::User && !m.is_summary && Some(i) != last_user {
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
                    },
                );
                changed = true;
            }
        }
    }

    // 4. Drop oldest complete turns (never the summary, never the last turn)
    //    until within budget.
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
        })
        .collect();
    trim_history(trim_input, profile, rolling_summary, None, counter).messages
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
        let out = trim_history(msgs, &p, None, None, &HeuristicTokenCounter);
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
        let out = trim_history(msgs, &p, None, None, &HeuristicTokenCounter);
        assert!(
            out.estimated_tokens <= 1280,
            "history must fit the usable window, got {}",
            out.estimated_tokens
        );
    }

    /// Measured feedback: when the engine reports a real prompt that overshot
    /// the usable ceiling, the next turn's budget drops by exactly that
    /// overshoot — this is what stops the runaway that ended conversations.
    #[test]
    fn a_real_prompt_over_the_ceiling_shrinks_the_next_budget_by_the_overshoot() {
        let p = profile_reserved(1200, 4096, 768); // usable = 3328
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();

        let baseline =
            trim_history(msgs.clone(), &p, None, None, &HeuristicTokenCounter).estimated_tokens;
        // Engine said the last prompt was 3,786 tokens — 458 over the ceiling.
        let corrected =
            trim_history(msgs, &p, None, Some(3786), &HeuristicTokenCounter).estimated_tokens;
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
        let out = trim_history(msgs, &p, None, Some(100_000), &HeuristicTokenCounter);
        assert!(out.estimated_tokens > 0, "must keep the current turn");
    }

    fn user(index: usize, text: &str) -> TrimMessage {
        TrimMessage {
            index,
            role: TrimRole::User,
            text: text.to_string(),
            is_summary: false,
        }
    }
    fn assistant(index: usize, text: &str) -> TrimMessage {
        TrimMessage {
            index,
            role: TrimRole::Assistant,
            text: text.to_string(),
            is_summary: false,
        }
    }
    fn tool(index: usize, text: &str) -> TrimMessage {
        TrimMessage {
            index,
            role: TrimRole::ToolResult,
            text: text.to_string(),
            is_summary: false,
        }
    }

    #[test]
    fn strips_stale_system_context_from_prior_user_messages_only() {
        let wrapped =
            "<system-context>\nToday is X\n</system-context>\n<user-message>hi</user-message>";
        let msgs = vec![user(0, wrapped), assistant(1, "hello"), user(2, wrapped)];
        let out = trim_history(msgs, &profile(10_000), None, None, &HeuristicTokenCounter);
        assert!(out.changed);
        assert!(!out.messages[0].text.contains("<system-context>"));
        assert!(out.messages[0]
            .text
            .contains("<user-message>hi</user-message>"));
        // The LAST user message keeps its block (current-turn injection).
        assert!(out.messages[2].text.contains("<system-context>"));
    }

    #[test]
    fn truncates_oversized_tool_results() {
        let big = format!("START{}END", "x".repeat(TOOL_RESULT_MAX_CHARS + 500));
        let msgs = vec![user(0, "check"), tool(1, &big), assistant(2, "done")];
        let out = trim_history(msgs, &profile(10_000), None, None, &HeuristicTokenCounter);
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
        let out = trim_history(msgs, &profile(100), None, None, &HeuristicTokenCounter);
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
        let out = trim_history(msgs, &profile(50), None, None, &HeuristicTokenCounter);
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
        );
        let twice = trim_history(
            once.messages.clone(),
            &profile(100),
            Some("sum"),
            None,
            &HeuristicTokenCounter,
        );
        assert!(!twice.changed, "second pass must be a no-op");
        assert_eq!(once.messages.len(), twice.messages.len());
    }

    #[test]
    fn unchanged_input_reports_changed_false() {
        let msgs = vec![user(0, "hi"), assistant(1, "hello")];
        let out = trim_history(msgs, &profile(10_000), None, None, &HeuristicTokenCounter);
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
        );
        assert_eq!(relaxed.dropped_turns, 0);
        let tightened = trim_history(
            msgs,
            &profile(300),
            None,
            Some(6144),
            &HeuristicTokenCounter,
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
}
