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

use super::context_budget::{CompactionProfile, TOOL_RESULT_MAX_CHARS};

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

    // 2. Truncate oversized tool results.
    for m in msgs.iter_mut() {
        if m.role == TrimRole::ToolResult && m.text.len() > TOOL_RESULT_MAX_CHARS {
            m.text.truncate(TOOL_RESULT_MAX_CHARS);
            m.text.push_str("\n[tool output truncated]");
            changed = true;
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
        let big = "x".repeat(TOOL_RESULT_MAX_CHARS + 500);
        let msgs = vec![user(0, "check"), tool(1, &big), assistant(2, "done")];
        let out = trim_history(msgs, &profile(10_000), None, None);
        assert!(out.changed);
        assert!(out.messages[1].text.len() < TOOL_RESULT_MAX_CHARS + 40);
        assert!(out.messages[1].text.ends_with("[tool output truncated]"));
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
}
