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
//! Token estimation is chars/4, optionally tightened by feedback: when the
//! previous turn's REAL prompt token count (from `TurnStats`) exceeded the
//! budget, the effective budget shrinks proportionally so the estimate error
//! self-corrects without a tokenizer dependency in pond-core.

use std::borrow::Cow;

use super::context_budget::{truncate_head_tail, CompactionProfile, TOOL_RESULT_MAX_CHARS};

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

fn estimate_tokens(messages: &[TrimMessage]) -> usize {
    messages.iter().map(|m| m.text.len() / 4 + 4).sum()
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
/// turn's engine-reported prompt size, used to tighten the chars/4 estimate.
pub fn trim_history(
    messages: Vec<TrimMessage>,
    profile: &CompactionProfile,
    rolling_summary: Option<&str>,
    last_real_prompt_tokens: Option<u32>,
) -> TrimOutcome {
    let mut changed = false;

    // Effective budget: shrink proportionally when the engine told us our
    // last estimate under-counted (real prompt exceeded the budget).
    let mut budget = profile.history_token_budget;
    if let Some(real) = last_real_prompt_tokens {
        let real = real as usize;
        if real > profile.context_window_tokens && profile.context_window_tokens > 0 {
            let ratio = profile.context_window_tokens as f32 / real as f32;
            budget = ((budget as f32) * ratio).max(64.0) as usize;
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
        let estimated = estimate_tokens(&msgs);
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

    let estimated_tokens = estimate_tokens(&msgs);
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
    trim_history(trim_input, profile, rolling_summary, None).messages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(history_budget: usize) -> CompactionProfile {
        CompactionProfile {
            compaction_threshold: 0.6,
            memory_token_budget: 200,
            max_memory_fragments: 3,
            system_prompt_budget: 1500,
            history_token_budget: history_budget,
            context_window_tokens: 3072,
        }
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
        let out = trim_history(msgs, &profile(10_000), None, None);
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
        let out = trim_history(msgs, &profile(10_000), None, None);
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
        let out = trim_history(msgs, &profile(10_000), Some("we discussed ducks"), None);
        assert!(out.messages[0].is_summary);
        assert!(out.messages[0].text.contains("<conversation-summary>"));
        assert!(out.messages[0].text.contains("we discussed ducks"));

        // Refresh replaces the body, does not duplicate.
        let out2 = trim_history(out.messages, &profile(10_000), Some("now geese"), None);
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
        let out = trim_history(msgs, &profile(100), None, None);
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
        let out = trim_history(msgs, &profile(50), None, None);
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
        let once = trim_history(msgs, &profile(100), Some("sum"), None);
        let twice = trim_history(once.messages.clone(), &profile(100), Some("sum"), None);
        assert!(!twice.changed, "second pass must be a no-op");
        assert_eq!(once.messages.len(), twice.messages.len());
    }

    #[test]
    fn unchanged_input_reports_changed_false() {
        let msgs = vec![user(0, "hi"), assistant(1, "hello")];
        let out = trim_history(msgs, &profile(10_000), None, None);
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
        let relaxed = trim_history(msgs.clone(), &profile(300), None, None);
        assert_eq!(relaxed.dropped_turns, 0);
        let tightened = trim_history(msgs, &profile(300), None, Some(6144));
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
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].role, TrimRole::Assistant);
        assert!(!out.iter().any(|m| m.text.contains("about to be sent")));
    }

    /// A conversation that is nothing but pending user messages replays as
    /// nothing at all — hydrating it would only duplicate the current turn.
    #[test]
    fn replay_of_only_user_messages_is_empty() {
        assert!(plan_replay(rows(&[(TrimRole::User, "hello")]), &profile(10_000), None).is_empty());
        assert!(plan_replay(vec![], &profile(10_000), None).is_empty());
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
        let first = plan_replay(input.clone(), &profile(10_000), Some("s"));
        let again: Vec<(TrimRole, String)> = first
            .iter()
            .filter(|m| !m.is_summary)
            .map(|m| (m.role, m.text.clone()))
            .collect();
        let second = plan_replay(again, &profile(10_000), Some("s"));
        assert_eq!(
            first.iter().map(|m| m.text.clone()).collect::<Vec<_>>(),
            second.iter().map(|m| m.text.clone()).collect::<Vec<_>>()
        );
    }
}
