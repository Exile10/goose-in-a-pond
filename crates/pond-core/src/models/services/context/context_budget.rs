//! Context budget management for constrained LLM inference.
//!
//! On Jetson Orin Nano with a 7B Q4 model, the context window is ~8K tokens.
//! After reserving space for the system prompt and the generated response,
//! roughly 2,500 tokens (~9,952 chars at 4 chars/token) are usable for history.
//!
//! `trim_to_budget()` walks messages newest-first and keeps messages until
//! the character budget is exhausted, then reverses to restore chronological order.
//! This ensures the most recent context is always preserved.

use crate::models::domain::message::{ChatMessage, Role};
use crate::models::domain::model_capabilities::ModelCapabilities;

const CHARS_PER_TOKEN: usize = 4;
const MIN_USABLE_HISTORY_CHARS: usize = 256;

// ── Compaction profile ──────────────────────────────────────────────────────

/// Per-model compaction parameters derived from the effective context window.
///
/// Goose's auto-compaction triggers at ~80% of its configured context_limit.
/// On tiny KV caches (3K Jetson, 8K macOS Metal), that 80% threshold still
/// leaves very little headroom. CompactionProfile tunes thresholds, memory
/// injection budgets, and prompt budgets so the agent operates comfortably
/// within the actual hardware limit.
///
/// Use [`CompactionProfile::from_context_window`] to derive the correct
/// profile from the effective context size reported by the provider adapter.
#[derive(Debug, Clone)]
pub struct CompactionProfile {
    /// Fraction of context at which proactive compaction should trigger (0.0-1.0).
    /// Goose uses this via GOOSE_CONTEXT_LIMIT; we also use it for GIAP-side
    /// budget calculations.
    pub compaction_threshold: f32,
    /// Max tokens to allocate for memory injection into the system prompt.
    pub memory_token_budget: usize,
    /// Max number of memory fragments to inject per turn.
    pub max_memory_fragments: usize,
    /// Max tokens for the full system prompt (base + extras + memories).
    pub system_prompt_budget: usize,
    /// Max tokens to allocate for conversation history injection (per request).
    pub history_token_budget: usize,
    /// Tokens held back for the model's OWN output — reasoning plus answer.
    ///
    /// Nothing else reserves this, and on a small window that omission is what
    /// ends conversations. Measured on the Orin (n_ctx 4096): the preamble is
    /// ~3,250 tokens, a couple of turns push the prompt to ~3,800, and then a
    /// `thinking` block of up to 306 tokens overruns the window MID-generation.
    /// llama.cpp returns ContextLengthExceeded ("Generation exhausted context
    /// window"), and goose answers that by compacting the conversation
    /// reactively — a path that does NOT consult GOOSE_AUTO_COMPACT_THRESHOLD,
    /// so GIAP's "we own compaction" setting cannot prevent it. The user sees
    /// their history replaced by a summary and an answer cut to one character.
    ///
    /// So the prompt must be budgeted against `context_window - this`, never
    /// against the raw window.
    pub output_reserve_tokens: usize,
    /// The effective context window this profile was derived from.
    pub context_window_tokens: usize,
    /// The window the PREAMBLE budgets were derived from.
    ///
    /// Equal to `context_window_tokens` except on providers whose preamble is
    /// re-prefilled locally every turn, where it is the clamped prompt-side
    /// window (`ContextGovernor::prompt_window`). Carrying both is what makes
    /// the asymmetry a property of the profile rather than of whichever caller
    /// remembered to clamp — see [`CompactionProfile::for_windows`].
    pub prompt_window_tokens: usize,
}

impl CompactionProfile {
    /// Tokens the whole PROMPT may occupy: the window minus the output reserve.
    ///
    /// This is the ceiling the engine's own reported `prompt_tokens` is compared
    /// against, so it covers preamble plus history. For the history budget
    /// alone, use [`CompactionProfile::usable_history_tokens`].
    pub fn usable_prompt_tokens(&self) -> usize {
        self.context_window_tokens
            .saturating_sub(self.output_reserve_tokens)
    }

    /// Tokens HISTORY may occupy: the usable prompt space, minus the preamble
    /// this profile has already promised to the system prompt and the injected
    /// memory block.
    ///
    /// `usable_prompt_tokens()` alone was never sufficient as a history clamp
    /// and P4's own notes said so: at 8,192 the profile declares 1,024 + 3,000 +
    /// 500 + 4,000 = 8,524 tokens against an 8,192-token window, and a clamp
    /// that subtracts only the output reserve lets history claim 7,168 of it on
    /// top of a preamble that is still going to be sent. The overrun that costs
    /// is the mid-generation one described on `output_reserve_tokens`.
    ///
    /// Saturating rather than floored: a preamble allowance larger than the
    /// window legitimately leaves no room for history at all, and
    /// `turn_trimmer` owns the floor that keeps the current turn alive.
    pub fn usable_history_tokens(&self) -> usize {
        self.usable_prompt_tokens()
            .saturating_sub(self.system_prompt_budget + self.memory_token_budget)
    }
}

/// One point on the budget curve: the profile that this exact window produces.
///
/// # Why these six, and not the four tiers
///
/// Four of them ARE the old tier values, at the windows where the old step
/// function returned them. The other two - 8,192 and 32,768 - are not tier
/// boundaries, and pinning them is the whole reason this change is a refactor
/// rather than a retune:
///
/// - **8,192 is the most-executed window in the system.** `prompt_window`
///   clamps every local provider to exactly 8,192, and both prompt-side call
///   sites (the compact-prompt decision and memory injection) pass the clamped
///   value here. Interpolating 4,096 -> 12,288 across it would have cut
///   `max_memory_fragments` from 5 to 4 and `memory_token_budget` from 500 to
///   350 on every Jetson and every macOS Metal turn.
/// - **32,768 is what the name heuristic hands to qwen and mistral**, and an
///   existing regression test pins it.
///
/// Because 8,192/12,288 and 32,768/65,536 carry identical values, those two
/// segments are FLAT: the whole of each old bucket's upper half answers exactly
/// as it did before. What changes is the interior of the lower halves, which is
/// the point - a 24K model and a 64K model no longer share a budget.
struct ProfileAnchor {
    window: usize,
    compaction_threshold: f32,
    memory_token_budget: usize,
    max_memory_fragments: usize,
    system_prompt_budget: usize,
    history_token_budget: usize,
    output_reserve_tokens: usize,
}

/// The budget curve, ascending by window. Below the first anchor and above the
/// last the curve is flat, which reproduces the old function's open-ended first
/// and last tiers.
static PROFILE_ANCHORS: [ProfileAnchor; 6] = [
    // Jetson-class. A thinking block alone measured 306 tokens on this device;
    // 768 covers reasoning plus a real answer, and it is the floor for every
    // smaller window too (invariant 2: the output reserve is never zero).
    ProfileAnchor {
        window: 4_096,
        compaction_threshold: 0.60,
        memory_token_budget: 200,
        max_memory_fragments: 3,
        system_prompt_budget: 1_500,
        history_token_budget: 1_200,
        output_reserve_tokens: 768,
    },
    // The local prompt clamp, and the macOS Metal default.
    ProfileAnchor {
        window: 8_192,
        compaction_threshold: 0.70,
        memory_token_budget: 500,
        max_memory_fragments: 5,
        system_prompt_budget: 3_000,
        history_token_budget: 4_000,
        output_reserve_tokens: 1_024,
    },
    // Old tier-2 ceiling. Same values as 8,192, so 8,192..12,288 is flat.
    ProfileAnchor {
        window: 12_288,
        compaction_threshold: 0.70,
        memory_token_budget: 500,
        max_memory_fragments: 5,
        system_prompt_budget: 3_000,
        history_token_budget: 4_000,
        output_reserve_tokens: 1_024,
    },
    // What the name heuristic gives qwen and mistral.
    ProfileAnchor {
        window: 32_768,
        compaction_threshold: 0.75,
        memory_token_budget: 1_500,
        max_memory_fragments: 10,
        system_prompt_budget: 6_000,
        history_token_budget: 20_000,
        output_reserve_tokens: 2_048,
    },
    // Old tier-3 ceiling. Same values as 32,768, so 32,768..65,536 is flat.
    ProfileAnchor {
        window: 65_536,
        compaction_threshold: 0.75,
        memory_token_budget: 1_500,
        max_memory_fragments: 10,
        system_prompt_budget: 6_000,
        history_token_budget: 20_000,
        output_reserve_tokens: 2_048,
    },
    // Large-context HTTP models (gemma-4 by name heuristic).
    ProfileAnchor {
        window: 128_000,
        compaction_threshold: 0.80,
        memory_token_budget: 4_000,
        max_memory_fragments: 15,
        system_prompt_budget: 10_000,
        history_token_budget: 80_000,
        output_reserve_tokens: 4_096,
    },
];

/// Interpolate a token budget. The `t <= 0.0` and `t >= 1.0` short-circuits make
/// anchor reproduction structural rather than a matter of floating-point luck:
/// landing exactly on an anchor must return that anchor's integer, not something
/// one ULP away that rounds the other direction.
fn lerp_budget(a: usize, b: usize, t: f64) -> usize {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    (a as f64 + (b as f64 - a as f64) * t).round() as usize
}

fn lerp_threshold(a: f32, b: f32, t: f64) -> f32 {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    (a as f64 + (b as f64 - a as f64) * t) as f32
}

impl CompactionProfile {
    /// Derive a compaction profile from the effective context window in tokens.
    ///
    /// Piecewise-linear over [`PROFILE_ANCHORS`], flat outside them. This
    /// replaced four hardcoded tiers, and the anchors ARE the old tier values,
    /// so every window the tiers were ever tested at answers identically - see
    /// `TIER_FIXTURES` in the tests, which is the guard for that claim.
    ///
    /// # What actually changed, and why it is a fix
    ///
    /// The old step function was correct at its four boundaries and
    /// over-committed everywhere in between: at 12,289 tokens it promised a
    /// 20,000-token history budget, a 6,000-token system prompt, 1,500 tokens of
    /// memory and a 2,048-token output reserve - 29,548 tokens of budget against
    /// a 12,289-token window, 2.4x over. Only `turn_trimmer`'s
    /// `min(usable_prompt_tokens())` clamp stood between that and a
    /// mid-generation `ContextLengthExceeded`, and that clamp does not subtract
    /// the system prompt or the memory block, so it was never sufficient.
    ///
    /// This matters most at 16,384, which is what the local model registry pins
    /// on the Orin: budgets there now sum to 12,729 against the 16,384 window
    /// instead of 29,548. Over 4,096..=200,000 the curve's budget sum is never
    /// larger than the tiers' was, its over-commitment set is a strict subset of
    /// theirs, and worst-case over-commitment falls from 2.404x to 1.041x.
    ///
    /// Every budget is monotonically non-decreasing in the window: raising
    /// `context_window_override` can never buy less of anything.
    pub fn from_context_window(context_tokens: usize) -> Self {
        let first = &PROFILE_ANCHORS[0];
        let last = &PROFILE_ANCHORS[PROFILE_ANCHORS.len() - 1];

        let (lo, hi, t) = if context_tokens <= first.window {
            (first, first, 0.0)
        } else if context_tokens >= last.window {
            (last, last, 0.0)
        } else {
            // The table is ascending and `context_tokens` is strictly inside it,
            // so this always finds an index >= 1.
            let i = PROFILE_ANCHORS
                .iter()
                .position(|a| a.window >= context_tokens)
                .expect("context_tokens is below the last anchor");
            let lo = &PROFILE_ANCHORS[i - 1];
            let hi = &PROFILE_ANCHORS[i];
            let t = (context_tokens - lo.window) as f64 / (hi.window - lo.window) as f64;
            (lo, hi, t)
        };

        Self {
            compaction_threshold: lerp_threshold(
                lo.compaction_threshold,
                hi.compaction_threshold,
                t,
            ),
            memory_token_budget: lerp_budget(lo.memory_token_budget, hi.memory_token_budget, t),
            max_memory_fragments: lerp_budget(lo.max_memory_fragments, hi.max_memory_fragments, t),
            system_prompt_budget: lerp_budget(lo.system_prompt_budget, hi.system_prompt_budget, t),
            history_token_budget: lerp_budget(lo.history_token_budget, hi.history_token_budget, t),
            output_reserve_tokens: lerp_budget(
                lo.output_reserve_tokens,
                hi.output_reserve_tokens,
                t,
            ),
            context_window_tokens: context_tokens,
            prompt_window_tokens: context_tokens,
        }
    }

    /// The asymmetric profile: history budgeted from the full window, preamble
    /// budgeted from the clamped prompt-side window.
    ///
    /// # Why the two halves are not the same number
    ///
    /// A context window has two halves with opposite cost curves. The preamble
    /// (system prompt, tool schemas, injected memories) is the KV prefix, so on
    /// a provider that re-prefills locally every turn, every token of it is paid
    /// again on every turn. The working set (conversation history) is paid too,
    /// but it is what makes the assistant remember you, and it is what gets
    /// thrown away first. So growing the window must buy working set, never
    /// preamble (PAI-3 invariant 1).
    ///
    /// Until this existed the asymmetry was enforced by the CALLER: two sites in
    /// `GooseAdapter` remembered to pass `ContextGovernor::prompt_window(..)`
    /// into `from_context_window`, and every other consumer got a profile whose
    /// preamble budgets scaled with the full window. That is the same shape as
    /// the four-paths-disagree defect PAI-3 exists to remove, one layer down.
    ///
    /// # What it does with the tokens the preamble is not allowed to have
    ///
    /// It gives them to history. The preamble fields come from the anchor curve
    /// at `prompt_window`; everything else comes from the curve at
    /// `context_window`; and the difference between the two preamble
    /// allowances is ADDED to `history_token_budget`. So the total budget is
    /// identical to `from_context_window(context_window)` - which is what keeps
    /// P4's "never promises more than the tiers did" property true - while the
    /// split between prefix and working set moves.
    ///
    /// Concretely, on a local provider whose window resolves to 32,768 the
    /// preamble stays at the 8,192 allowance (3,000 + 500) instead of growing to
    /// 6,000 + 1,500, and the 4,000 tokens that frees go to history: 20,000 ->
    /// 24,000. That is the measurable claim of this phase - flat TTFT, more
    /// retained history.
    ///
    /// When `prompt_window >= context_window` there is nothing to clamp and
    /// nothing to redistribute, so this is exactly `from_context_window`. HTTP
    /// providers take that path: their preamble is not paid for in local
    /// prefill.
    pub fn for_windows(context_window: usize, prompt_window: usize) -> Self {
        let full = Self::from_context_window(context_window);
        if prompt_window >= context_window {
            return full;
        }
        let capped = Self::from_context_window(prompt_window);
        let freed = (full.system_prompt_budget + full.memory_token_budget)
            .saturating_sub(capped.system_prompt_budget + capped.memory_token_budget);
        Self {
            memory_token_budget: capped.memory_token_budget,
            max_memory_fragments: capped.max_memory_fragments,
            system_prompt_budget: capped.system_prompt_budget,
            history_token_budget: full.history_token_budget + freed,
            prompt_window_tokens: prompt_window,
            ..full
        }
    }

    /// Whether the system prompt should use a compact format.
    ///
    /// Returns true when the window is small enough that verbose tool
    /// descriptions and detailed instructions waste precious tokens.
    ///
    /// Reads the PROMPT window, not the context window: this is a preamble
    /// decision, and on a local provider a 32K KV cache must not buy a more
    /// verbose prefix. Under `from_context_window` the two are the same number,
    /// so the 12288 boundary is unmoved for every existing caller.
    ///
    /// Deliberately NOT interpolated. Everything else on this profile is a
    /// budget and answers "how much"; this one is a format switch and answers
    /// "which". A continuous curve through a boolean has no meaning.
    pub fn use_compact_prompt(&self) -> bool {
        self.prompt_window_tokens <= 12288
    }
}

/// Calculate available history budget in characters after system prompt and tool schema overhead.
///
/// The returned value is the upper bound for the total length of message
/// content that may be injected as conversation history. Always returns at
/// least [`MIN_USABLE_HISTORY_CHARS`] so the agent can carry at least the
/// most recent turn even when overhead is large.
pub fn available_history_chars(
    profile: &CompactionProfile,
    system_prompt_chars: usize,
    tool_schema_chars: usize,
) -> usize {
    let total_budget_chars = profile.history_token_budget * CHARS_PER_TOKEN;
    let overhead = system_prompt_chars + tool_schema_chars;
    total_budget_chars
        .saturating_sub(overhead)
        .max(MIN_USABLE_HISTORY_CHARS)
}

/// Maximum assistant tool-output size kept verbatim in history.
pub const TOOL_RESULT_MAX_CHARS: usize = 1_500;

/// Shrink an oversized tool result to `max_chars`-ish, keeping BOTH ends.
///
/// Returns `None` when `text` already fits, so callers can skip rewriting.
///
/// Head-only truncation loses exactly the part that usually carries the answer:
/// a tool result's tail holds totals, the last log lines, the closing summary,
/// the "N more results" count. Keeping ~60% head and ~40% tail preserves the
/// shape of the payload (so the model can still tell what it is looking at) and
/// the conclusion, and the marker states how much is missing so the model can
/// call the tool again with a narrower query instead of assuming it saw
/// everything.
///
/// Always splits on char boundaries; the result can exceed `max_chars` by the
/// length of the marker, which is the honest trade for saying how much was cut.
pub fn truncate_head_tail(text: &str, max_chars: usize) -> Option<String> {
    if text.len() <= max_chars {
        return None;
    }
    // Degenerate budgets: a head-only cut is all that fits.
    if max_chars < 64 {
        let mut end = max_chars.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        return Some(format!(
            "{}\n[... truncated {} chars ...]",
            &text[..end],
            text.len() - end
        ));
    }

    let head_len = max_chars * 3 / 5;
    let tail_len = max_chars - head_len;

    let mut head_end = head_len;
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len().saturating_sub(tail_len);
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    // A pathological multi-byte boundary walk could cross the head — then there
    // is nothing left to keep from the tail.
    if tail_start <= head_end {
        return Some(format!(
            "{}\n[... truncated {} chars ...]",
            &text[..head_end],
            text.len() - head_end
        ));
    }

    let dropped = tail_start - head_end;
    Some(format!(
        "{}\n[... truncated {dropped} chars ...]\n{}",
        &text[..head_end],
        &text[tail_start..]
    ))
}

/// Approximate total context window in characters (8K tokens × 4 chars/token).
pub const MAX_CONTEXT_CHARS: usize = 12_000;

/// Characters reserved for the LLM's generated response.
pub const RESERVE_FOR_RESPONSE_CHARS: usize = 2_048;

/// Characters available for conversation history after reserving for response.
pub const USABLE_HISTORY_CHARS: usize = MAX_CONTEXT_CHARS - RESERVE_FOR_RESPONSE_CHARS;

fn truncate_at_byte_budget(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }

    let mut end = max_bytes.min(content.len());
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    content[..end].to_string()
}

fn trim_to_char_budget(
    messages: Vec<ChatMessage>,
    usable_history_chars: usize,
) -> Vec<ChatMessage> {
    let mut kept: Vec<ChatMessage> = Vec::new();
    let mut remaining = usable_history_chars;

    for msg in messages.into_iter().rev() {
        let len = msg.content.len();
        if remaining == 0 {
            break;
        }
        if len <= remaining {
            remaining -= len;
            kept.push(msg);
        } else if kept.is_empty() {
            // First (most recent) message exceeds budget — truncate rather than drop.
            let truncated = truncate_at_byte_budget(&msg.content, remaining);
            kept.push(ChatMessage {
                content: truncated,
                ..msg
            });
            break;
        } else {
            // Later messages don't fit — stop here.
            break;
        }
    }

    kept.reverse();
    kept
}

/// Truncate oversized assistant tool outputs before history trimming.
///
/// This targets assistant messages that look like structured tool output
/// payloads (JSON/code blocks) and keeps the first [`TOOL_RESULT_MAX_CHARS`]
/// characters plus a small marker.
pub fn truncate_tool_outputs(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    messages
        .into_iter()
        .map(|msg| {
            if msg.role != Role::Assistant || msg.content.len() <= TOOL_RESULT_MAX_CHARS {
                return msg;
            }

            let looks_like_tool_output = msg.content.contains("```json")
                || msg.content.contains("\"tool\"")
                || msg.content.contains("\"result\"")
                || (msg.content.contains('{') && msg.content.contains('}'));

            if !looks_like_tool_output {
                return msg;
            }

            let mut truncated = truncate_at_byte_budget(&msg.content, TOOL_RESULT_MAX_CHARS);
            truncated.push_str("\n\n[tool output truncated]");
            ChatMessage {
                content: truncated,
                ..msg
            }
        })
        .collect()
}

/// Trim a message list to fit within a context-token budget.
///
/// The token limit is converted to an approximate char budget via 4 chars/token,
/// with [`RESERVE_FOR_RESPONSE_CHARS`] held back for model generation.
pub fn trim_to_budget_with_limit(
    messages: Vec<ChatMessage>,
    context_limit_tokens: usize,
) -> Vec<ChatMessage> {
    let usable_history_chars = context_limit_tokens
        .saturating_mul(CHARS_PER_TOKEN)
        .saturating_sub(RESERVE_FOR_RESPONSE_CHARS)
        .max(MIN_USABLE_HISTORY_CHARS);

    trim_to_char_budget(messages, usable_history_chars)
}

/// Trim a message list to fit within [`USABLE_HISTORY_CHARS`].
///
/// Walks messages newest-first, keeping each message until the budget is
/// exhausted.  Returns the surviving messages in chronological (oldest-first)
/// order so they can be passed directly to `LlmProvider::complete()`.
///
/// An individual message that exceeds the entire budget on its own is
/// truncated to `USABLE_HISTORY_CHARS` characters so the caller always
/// receives at least one message.
pub fn trim_to_budget(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    trim_to_char_budget(messages, USABLE_HISTORY_CHARS)
}

/// Trim using the model's actual context window.
///
/// Reserves 20% for the system prompt and generation headroom (minimum 2048 tokens).
/// When `override_tokens` is non-zero, it caps the context window to that value
/// (useful for memory-constrained deployments like Jetson 8GB).
pub fn trim_to_budget_for_model(
    messages: Vec<ChatMessage>,
    capabilities: &ModelCapabilities,
    override_tokens: u32,
) -> Vec<ChatMessage> {
    let token_limit = if override_tokens > 0 {
        override_tokens.min(capabilities.context_window_tokens) as usize
    } else {
        capabilities.context_window_tokens as usize
    };
    // Reserve 20% for system prompt + generation headroom, min 2048 tokens
    let reserved = (token_limit / 5).max(2048);
    let effective = token_limit.saturating_sub(reserved).max(256);
    trim_to_budget_with_limit(messages, effective)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::message::Role;

    fn msg(content: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: content.to_string(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    fn total_chars(msgs: &[ChatMessage]) -> usize {
        msgs.iter().map(|m| m.content.len()).sum()
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(trim_to_budget(vec![]).is_empty());
    }

    // ── truncate_head_tail (C3) ──────────────────────────────────────────

    #[test]
    fn text_within_budget_is_left_alone() {
        assert!(truncate_head_tail("short", TOOL_RESULT_MAX_CHARS).is_none());
        let exact = "x".repeat(TOOL_RESULT_MAX_CHARS);
        assert!(truncate_head_tail(&exact, TOOL_RESULT_MAX_CHARS).is_none());
    }

    /// The point of head+tail: the CONCLUSION at the end survives, which a
    /// head-only truncation would have thrown away.
    #[test]
    fn both_ends_survive_and_the_marker_states_the_loss() {
        let text = format!("HEAD-MARKER{}TAIL-MARKER", "x".repeat(50_000));
        let out = truncate_head_tail(&text, TOOL_RESULT_MAX_CHARS).unwrap();
        assert!(out.starts_with("HEAD-MARKER"), "{}", &out[..40]);
        assert!(out.ends_with("TAIL-MARKER"), "{}", &out[out.len() - 40..]);
        assert!(out.contains("[... truncated "));
        // Far smaller than the original, and close to the budget.
        assert!(out.len() < TOOL_RESULT_MAX_CHARS + 64, "len {}", out.len());
        // The stated loss is accurate.
        let dropped: usize = out
            .split("[... truncated ")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|s| s.parse().ok())
            .unwrap();
        assert_eq!(
            dropped,
            text.len() - (out.len() - format!("\n[... truncated {dropped} chars ...]\n").len())
        );
    }

    #[test]
    fn multibyte_text_is_never_split_mid_char() {
        // Every char is 4 bytes, so naive byte slicing would panic.
        let text = "\u{1F600}".repeat(2_000);
        let out = truncate_head_tail(&text, TOOL_RESULT_MAX_CHARS).unwrap();
        assert!(out.contains("[... truncated "));
        // Round-trips as valid UTF-8 with no replacement chars introduced.
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn a_tiny_budget_degrades_to_a_head_cut() {
        let text = "y".repeat(500);
        let out = truncate_head_tail(&text, 10).unwrap();
        assert!(out.starts_with("yyyyyyyyyy"));
        assert!(out.contains("[... truncated 490 chars ...]"));
    }

    #[test]
    fn small_history_passes_through_unchanged() {
        let messages = vec![msg("Hello"), msg("How are you?"), msg("Good thanks")];
        let result = trim_to_budget(messages.clone());
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].content, "Hello");
        assert_eq!(result[2].content, "Good thanks");
    }

    #[test]
    fn large_history_is_trimmed_to_budget() {
        // 200 messages × 200 chars each = 40_000 chars >> USABLE_HISTORY_CHARS (9_952)
        let messages: Vec<ChatMessage> = (0..200).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget(messages);
        assert!(total_chars(&result) <= USABLE_HISTORY_CHARS);
        // Should keep at least 1 message
        assert!(!result.is_empty());
    }

    #[test]
    fn most_recent_messages_are_preserved() {
        // Fill budget with old junk, then add a recent message that fits
        let mut messages: Vec<ChatMessage> = (0..60).map(|_| msg(&"a".repeat(200))).collect();
        messages.push(msg("final important message"));

        let result = trim_to_budget(messages);
        // The last message should always be in the result
        assert_eq!(result.last().unwrap().content, "final important message");
    }

    #[test]
    fn single_oversized_message_is_truncated_not_dropped() {
        let big = msg(&"z".repeat(USABLE_HISTORY_CHARS + 1000));
        let result = trim_to_budget(vec![big]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content.len(), USABLE_HISTORY_CHARS);
    }

    #[test]
    fn chronological_order_preserved_after_trim() {
        let messages: Vec<ChatMessage> =
            (0..10u32).map(|i| msg(&format!("message-{}", i))).collect();
        let result = trim_to_budget(messages);
        // Result must be in original order (oldest first)
        for w in result.windows(2) {
            let a: u32 = w[0]
                .content
                .strip_prefix("message-")
                .unwrap()
                .parse()
                .unwrap();
            let b: u32 = w[1]
                .content
                .strip_prefix("message-")
                .unwrap()
                .parse()
                .unwrap();
            assert!(a < b, "messages out of order: {} >= {}", a, b);
        }
    }

    #[test]
    fn budget_exactly_full_keeps_all() {
        // Each message is exactly USABLE_HISTORY_CHARS / 4 chars
        let chunk = USABLE_HISTORY_CHARS / 4;
        let messages: Vec<ChatMessage> = (0..4).map(|_| msg(&"m".repeat(chunk))).collect();
        let result = trim_to_budget(messages);
        assert_eq!(result.len(), 4);
        assert_eq!(total_chars(&result), USABLE_HISTORY_CHARS);
    }

    #[test]
    fn trim_to_budget_with_limit_respects_smaller_context() {
        // context_limit_tokens=1024 -> usable chars = 4096-2048=2048
        let messages: Vec<ChatMessage> = (0..20).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget_with_limit(messages, 1024);
        assert!(total_chars(&result) <= 2048);
    }

    #[test]
    fn truncate_tool_outputs_truncates_large_assistant_payloads() {
        let messages = vec![
            ChatMessage {
                role: Role::Assistant,
                content: format!(
                    "{{\"tool\":\"weather\",\"result\":\"{}\"}}",
                    "x".repeat(TOOL_RESULT_MAX_CHARS + 300)
                ),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            msg("normal user message"),
        ];

        let result = truncate_tool_outputs(messages);
        assert_eq!(result.len(), 2);
        assert!(result[0].content.len() <= TOOL_RESULT_MAX_CHARS + 40);
        assert!(result[0].content.contains("[tool output truncated]"));
        assert_eq!(result[1].content, "normal user message");
    }

    #[test]
    fn truncate_tool_outputs_keeps_regular_assistant_text() {
        let plain_assistant = ChatMessage {
            role: Role::Assistant,
            content: "This is a normal answer without tool payload markers.".to_string(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        };
        let result = truncate_tool_outputs(vec![plain_assistant.clone()]);
        assert_eq!(result[0].content, plain_assistant.content);
    }

    #[test]
    fn trim_for_model_uses_reported_context_window() {
        // 128K context → 80% usable = ~102K tokens → ~409K chars
        let caps = ModelCapabilities {
            context_window_tokens: 128_000,
            ..Default::default()
        };
        let messages: Vec<ChatMessage> = (0..200).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget_for_model(messages.clone(), &caps, 0);
        // With 128K context, all 200 messages (40K chars) should fit easily
        assert_eq!(result.len(), 200);
    }

    #[test]
    fn trim_for_model_with_small_context() {
        // 4K context → 80% = ~3.2K tokens → ~12.8K chars - 2048 reserve
        let caps = ModelCapabilities::default(); // 4096 tokens
        let messages: Vec<ChatMessage> = (0..200).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget_for_model(messages, &caps, 0);
        // Should trim significantly — 40K chars won't fit in ~6K usable
        assert!(result.len() < 200);
        assert!(!result.is_empty());
    }

    #[test]
    fn trim_for_model_override_caps_context() {
        // Model reports 128K but override limits to 4K
        let caps = ModelCapabilities {
            context_window_tokens: 128_000,
            ..Default::default()
        };
        let messages: Vec<ChatMessage> = (0..200).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget_for_model(messages, &caps, 4096);
        // Override to 4K should trim just like the small context case
        assert!(result.len() < 200);
    }

    // ── CompactionProfile tests ─────────────────────────────────────────

    #[test]
    fn compaction_profile_jetson_3k() {
        let p = CompactionProfile::from_context_window(3072);
        assert!((p.compaction_threshold - 0.60).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 200);
        assert_eq!(p.max_memory_fragments, 3);
        assert_eq!(p.system_prompt_budget, 1500);
        assert_eq!(p.history_token_budget, 1200);
        assert!(p.use_compact_prompt());
    }

    #[test]
    fn compaction_profile_macos_8k() {
        let p = CompactionProfile::from_context_window(8192);
        assert!((p.compaction_threshold - 0.70).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 500);
        assert_eq!(p.max_memory_fragments, 5);
        assert_eq!(p.system_prompt_budget, 3000);
        assert_eq!(p.history_token_budget, 4000);
        assert!(p.use_compact_prompt());
    }

    #[test]
    fn compaction_profile_32k() {
        let p = CompactionProfile::from_context_window(32768);
        assert!((p.compaction_threshold - 0.75).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 1500);
        assert_eq!(p.max_memory_fragments, 10);
        assert_eq!(p.system_prompt_budget, 6000);
        assert_eq!(p.history_token_budget, 20000);
        assert!(!p.use_compact_prompt());
    }

    #[test]
    fn compaction_profile_128k() {
        let p = CompactionProfile::from_context_window(128_000);
        assert!((p.compaction_threshold - 0.80).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 4000);
        assert_eq!(p.max_memory_fragments, 15);
        assert_eq!(p.system_prompt_budget, 10000);
        assert_eq!(p.history_token_budget, 80000);
        assert!(!p.use_compact_prompt());
    }

    #[test]
    fn available_history_chars_subtracts_overhead() {
        let profile = CompactionProfile::from_context_window(8192);
        // 4000 tokens * 4 chars/token = 16000 chars total budget
        let avail = available_history_chars(&profile, 1000, 500);
        assert_eq!(avail, 16_000 - 1500);
    }

    #[test]
    fn available_history_chars_clamps_to_min_when_overhead_exceeds_budget() {
        let profile = CompactionProfile::from_context_window(3072);
        // 1200 * 4 = 4800 budget, overhead 10000 → should clamp to MIN_USABLE_HISTORY_CHARS
        let avail = available_history_chars(&profile, 6000, 4000);
        assert_eq!(avail, MIN_USABLE_HISTORY_CHARS);
    }

    #[test]
    fn compaction_profile_boundary_4096() {
        // 4096 is the upper boundary of the Jetson tier
        let p = CompactionProfile::from_context_window(4096);
        assert!((p.compaction_threshold - 0.60).abs() < f32::EPSILON);
        assert_eq!(p.max_memory_fragments, 3);
    }

    #[test]
    fn compaction_profile_boundary_12288() {
        // 12288 is the upper boundary of the macOS tier
        let p = CompactionProfile::from_context_window(12288);
        assert!((p.compaction_threshold - 0.70).abs() < f32::EPSILON);
        assert_eq!(p.max_memory_fragments, 5);
    }

    #[test]
    fn compaction_profile_stores_context_window() {
        let p = CompactionProfile::from_context_window(8192);
        assert_eq!(p.context_window_tokens, 8192);
    }
    // ── P4: the continuous profile curve, with the tiers as fixtures ─────

    /// The four discrete tiers, verbatim as they were before P4. Kept as the
    /// reference implementation so the properties below are checked against the
    /// real prior behaviour rather than against numbers somebody transcribed.
    fn tiers_before_p4(context_tokens: usize) -> (f32, usize, usize, usize, usize, usize) {
        if context_tokens <= 4096 {
            (0.60, 200, 3, 1500, 1200, 768)
        } else if context_tokens <= 12288 {
            (0.70, 500, 5, 3000, 4000, 1024)
        } else if context_tokens <= 65536 {
            (0.75, 1500, 10, 6000, 20000, 2048)
        } else {
            (0.80, 4000, 15, 10000, 80000, 4096)
        }
    }

    fn budget_sum(p: &CompactionProfile) -> usize {
        p.output_reserve_tokens
            + p.system_prompt_budget
            + p.memory_token_budget
            + p.history_token_budget
    }

    /// Every window the discrete tiers were pinned at, with the values they
    /// produced. This table IS the phase: the machinery changed, these answers
    /// did not.
    ///
    /// It is deliberately wider than the four tier boundaries. 8,192 and 32,768
    /// are not boundaries, but 8,192 is what `ContextGovernor::prompt_window`
    /// hands every local provider and 32,768 is what the name heuristic hands
    /// qwen and mistral - so a curve that only honoured the boundaries would
    /// have shipped a real on-device change under a refactor's name.
    const TIER_FIXTURES: &[(usize, f32, usize, usize, usize, usize, usize)] = &[
        // window,   threshold, memory, frags, system, history, reserve
        (0, 0.60, 200, 3, 1500, 1200, 768),
        (3_072, 0.60, 200, 3, 1500, 1200, 768),
        (4_096, 0.60, 200, 3, 1500, 1200, 768),
        (8_192, 0.70, 500, 5, 3000, 4000, 1024),
        (12_288, 0.70, 500, 5, 3000, 4000, 1024),
        (32_768, 0.75, 1500, 10, 6000, 20000, 2048),
        (65_536, 0.75, 1500, 10, 6000, 20000, 2048),
        (128_000, 0.80, 4000, 15, 10000, 80000, 4096),
        (200_000, 0.80, 4000, 15, 10000, 80000, 4096),
    ];

    #[test]
    fn the_curve_reproduces_every_tier_fixture_exactly() {
        for &(w, thr, mem, frags, sys, hist, reserve) in TIER_FIXTURES {
            let p = CompactionProfile::from_context_window(w);
            assert!(
                (p.compaction_threshold - thr).abs() < 1e-6,
                "window {w}: compaction_threshold {} != {thr}",
                p.compaction_threshold
            );
            assert_eq!(
                p.memory_token_budget, mem,
                "window {w}: memory_token_budget"
            );
            assert_eq!(
                p.max_memory_fragments, frags,
                "window {w}: max_memory_fragments"
            );
            assert_eq!(
                p.system_prompt_budget, sys,
                "window {w}: system_prompt_budget"
            );
            assert_eq!(
                p.history_token_budget, hist,
                "window {w}: history_token_budget"
            );
            assert_eq!(
                p.output_reserve_tokens, reserve,
                "window {w}: output_reserve_tokens"
            );
            assert_eq!(
                p.context_window_tokens, w,
                "window {w}: context_window_tokens"
            );
        }
    }

    /// The safety property. The curve is allowed to differ from the tiers in the
    /// interior - that is the point - but it may never promise MORE budget than
    /// the tiers already did, at any window. Checked against the old function
    /// itself, at every integer window in the range that matters.
    #[test]
    fn the_curve_never_promises_more_budget_than_the_tiers_did() {
        for w in 4_096..=200_000usize {
            let p = CompactionProfile::from_context_window(w);
            let (_, mem, _, sys, hist, reserve) = tiers_before_p4(w);
            let before = mem + sys + hist + reserve;
            let after = budget_sum(&p);
            assert!(
                after <= before,
                "window {w}: curve promises {after} tokens, tiers promised {before}"
            );
        }
    }

    /// The tiers over-committed at every window that was not a boundary. The
    /// curve may still over-commit - the 8,192 fixture itself sums to 8,524,
    /// and correcting THAT is P5's asymmetric budgeting, not this phase - but
    /// wherever it does, the tiers did too, and by more.
    #[test]
    fn the_curve_over_commits_strictly_less_often_than_the_tiers() {
        let mut curve_violations = 0usize;
        let mut tier_violations = 0usize;
        for w in 4_096..=200_000usize {
            let p = CompactionProfile::from_context_window(w);
            let (_, mem, _, sys, hist, reserve) = tiers_before_p4(w);
            let tier_sum = mem + sys + hist + reserve;
            if budget_sum(&p) > w {
                curve_violations += 1;
                assert!(
                    tier_sum > w,
                    "window {w}: the curve over-commits where the tiers did not"
                );
            }
            if tier_sum > w {
                tier_violations += 1;
            }
        }
        assert_eq!(curve_violations, 2_116, "curve over-commitment count");
        assert_eq!(tier_violations, 54_245, "tier over-commitment count");
    }

    /// The Orin's real operating point, which no tier fixture covers: the local
    /// model registry pins n_ctx at 16,384 there. The tiers put it in the 32K
    /// bucket and declared 29,548 tokens of budget against a 16,384-token
    /// window - 1.8x over, with only `turn_trimmer`'s history clamp (which does
    /// not subtract the system prompt or the memory block) standing between that
    /// and a mid-generation context overrun.
    #[test]
    fn the_orins_pinned_window_stops_promising_more_than_the_window_holds() {
        let p = CompactionProfile::from_context_window(16_384);
        let sum = budget_sum(&p);
        assert!(
            sum <= p.context_window_tokens,
            "budgets sum to {sum} against a {}-token window",
            p.context_window_tokens
        );
        assert_eq!(sum, 12_729, "the Orin budget sum moved; re-derive it");
        assert_eq!(p.output_reserve_tokens, 1_229);
        assert_eq!(p.system_prompt_budget, 3_600);
        assert_eq!(p.memory_token_budget, 700);
        assert_eq!(p.max_memory_fragments, 6);
        assert_eq!(p.history_token_budget, 7_200);

        // What it was before, for the record.
        let (_, mem, _, sys, hist, reserve) = tiers_before_p4(16_384);
        assert_eq!(mem + sys + hist + reserve, 29_548);
    }

    /// The stated purpose of the phase, as an assertion.
    #[test]
    fn a_24k_model_and_a_64k_model_no_longer_share_a_bucket() {
        let k24 = CompactionProfile::from_context_window(24_576);
        let k64 = CompactionProfile::from_context_window(65_536);
        assert!(
            k24.history_token_budget < k64.history_token_budget,
            "24K got {} history tokens, 64K got {}",
            k24.history_token_budget,
            k64.history_token_budget
        );
        assert_eq!(k24.history_token_budget, 13_600);
        assert_eq!(k64.history_token_budget, 20_000);
    }

    /// Invariant 2 of the design: the output reserve is never zero and never
    /// optional. It is the only thing between a long thinking block and a
    /// mid-generation context overrun.
    #[test]
    fn the_output_reserve_is_never_below_its_floor_at_any_window() {
        for w in [0, 1, 512, 3_072, 4_096, 6_000, 8_192, 100_000, 1_000_000] {
            let p = CompactionProfile::from_context_window(w);
            assert!(
                p.output_reserve_tokens >= 768,
                "window {w}: reserve {}",
                p.output_reserve_tokens
            );
        }
    }

    /// A bigger window must never buy less of anything. Without this, raising
    /// `context_window_override` by one token could cost the user history.
    #[test]
    fn every_budget_is_non_decreasing_in_the_window() {
        let mut prev = CompactionProfile::from_context_window(4_096);
        for w in 4_097..=200_000usize {
            let p = CompactionProfile::from_context_window(w);
            assert!(
                p.compaction_threshold >= prev.compaction_threshold,
                "threshold at {w}"
            );
            assert!(
                p.memory_token_budget >= prev.memory_token_budget,
                "memory at {w}"
            );
            assert!(
                p.max_memory_fragments >= prev.max_memory_fragments,
                "fragments at {w}"
            );
            assert!(
                p.system_prompt_budget >= prev.system_prompt_budget,
                "system at {w}"
            );
            assert!(
                p.history_token_budget >= prev.history_token_budget,
                "history at {w}"
            );
            assert!(
                p.output_reserve_tokens >= prev.output_reserve_tokens,
                "reserve at {w}"
            );
            prev = p;
        }
    }

    /// `use_compact_prompt` is a bool, so it has no continuous form. It stays a
    /// hard step at 12,288 and must not be quietly folded into the curve.
    #[test]
    fn use_compact_prompt_is_still_a_hard_step_at_12288() {
        assert!(CompactionProfile::from_context_window(12_288).use_compact_prompt());
        assert!(!CompactionProfile::from_context_window(12_289).use_compact_prompt());
    }

    // ── P5: asymmetric budgeting - preamble capped, working set scaled ───

    /// The phase, as one assertion.
    ///
    /// A local provider's prompt window is clamped to 8,192 whatever the KV
    /// cache holds, so quadrupling the window from 8,192 to 32,768 must buy
    /// HISTORY and nothing else. If the preamble grows too, TTFT grows with it
    /// permanently - it is the KV prefix, re-prefilled every turn (invariant 1).
    #[test]
    fn growing_the_window_buys_history_and_never_preamble() {
        let small = CompactionProfile::for_windows(8_192, 8_192);
        let big = CompactionProfile::for_windows(32_768, 8_192);

        assert_eq!(
            big.system_prompt_budget, small.system_prompt_budget,
            "a 4x window bought a bigger system prompt: {} vs {}",
            big.system_prompt_budget, small.system_prompt_budget
        );
        assert_eq!(
            big.memory_token_budget, small.memory_token_budget,
            "a 4x window bought a bigger memory block: {} vs {}",
            big.memory_token_budget, small.memory_token_budget
        );
        assert_eq!(
            big.max_memory_fragments, small.max_memory_fragments,
            "a 4x window bought more memory fragments"
        );
        assert_eq!(
            big.use_compact_prompt(),
            small.use_compact_prompt(),
            "a 4x window flipped the prompt to the verbose tier"
        );

        assert!(
            big.history_token_budget > small.history_token_budget,
            "a 4x window bought no extra history: {} vs {}",
            big.history_token_budget,
            small.history_token_budget
        );
        // The exact numbers, so a silent re-tune is visible in the diff.
        assert_eq!(small.history_token_budget, 4_000);
        assert_eq!(big.history_token_budget, 24_000);
        assert_eq!(big.system_prompt_budget, 3_000);
        assert_eq!(big.memory_token_budget, 500);
    }

    /// The redistribution is exactly that - a redistribution. Nothing is
    /// invented, so P4's "never promises more budget than the tiers did"
    /// property survives untouched.
    #[test]
    fn capping_the_preamble_moves_tokens_to_history_and_creates_none() {
        for window in [8_193usize, 12_288, 16_384, 32_768, 65_536, 128_000] {
            let symmetric = CompactionProfile::from_context_window(window);
            let asymmetric = CompactionProfile::for_windows(window, 8_192);
            assert_eq!(
                budget_sum(&asymmetric),
                budget_sum(&symmetric),
                "window {window}: the split changed the TOTAL budget"
            );
            assert!(
                asymmetric.history_token_budget >= symmetric.history_token_budget,
                "window {window}: capping the preamble cost history"
            );
        }
    }

    /// The symmetric case must be bit-identical to the old constructor, or every
    /// HTTP provider silently re-tunes. `prompt_window >= context_window` is the
    /// path they take.
    #[test]
    fn an_unclamped_prompt_window_reproduces_from_context_window_exactly() {
        for &(w, ..) in TIER_FIXTURES {
            let a = CompactionProfile::from_context_window(w);
            for prompt in [w, w + 1, w * 2 + 1] {
                let b = CompactionProfile::for_windows(w, prompt);
                assert_eq!(budget_sum(&a), budget_sum(&b), "window {w} prompt {prompt}");
                assert_eq!(a.history_token_budget, b.history_token_budget, "window {w}");
                assert_eq!(a.system_prompt_budget, b.system_prompt_budget, "window {w}");
                assert_eq!(a.memory_token_budget, b.memory_token_budget, "window {w}");
                assert_eq!(a.max_memory_fragments, b.max_memory_fragments, "window {w}");
                assert_eq!(a.prompt_window_tokens, b.prompt_window_tokens, "window {w}");
                assert_eq!(a.use_compact_prompt(), b.use_compact_prompt(), "window {w}");
            }
        }
    }

    /// Invariant 1 as a range property rather than a spot check: across every
    /// window a local provider can resolve to, the preamble allowance is frozen
    /// at the clamp's values.
    #[test]
    fn the_preamble_allowance_is_flat_across_every_clamped_window() {
        let clamp = 8_192usize;
        let reference = CompactionProfile::from_context_window(clamp);
        for window in (clamp..=200_000).step_by(97) {
            let p = CompactionProfile::for_windows(window, clamp.min(window));
            assert_eq!(
                p.system_prompt_budget, reference.system_prompt_budget,
                "window {window}"
            );
            assert_eq!(
                p.memory_token_budget, reference.memory_token_budget,
                "window {window}"
            );
            assert_eq!(
                p.max_memory_fragments, reference.max_memory_fragments,
                "window {window}"
            );
            assert!(p.use_compact_prompt(), "window {window}");
        }
    }

    /// The over-commitment P4 deferred here. At 8,192 the declared budgets sum
    /// to 8,524 against an 8,192-token window, and the old history clamp
    /// (`usable_prompt_tokens`, window minus reserve only) let history claim
    /// 7,168 of that on top of a preamble that was still going to be sent.
    #[test]
    fn the_history_ceiling_subtracts_the_preamble_the_prompt_ceiling_does_not() {
        let p = CompactionProfile::from_context_window(8_192);
        // Unchanged: this is the ceiling the engine's own prompt_tokens is
        // measured against, so it must still cover preamble plus history.
        assert_eq!(p.usable_prompt_tokens(), 8_192 - 1_024);
        // New: history alone may not claim the preamble's room.
        assert_eq!(p.usable_history_tokens(), 7_168 - 3_000 - 500);
        assert!(
            p.usable_history_tokens() < p.history_token_budget,
            "the declared 4,000-token history budget was payable after all"
        );
    }

    /// A preamble allowance larger than the whole window leaves history nothing,
    /// and must saturate rather than wrap. `turn_trimmer` owns the floor that
    /// keeps the current turn alive.
    #[test]
    fn a_preamble_bigger_than_the_window_leaves_zero_history_room() {
        let p = CompactionProfile::from_context_window(1_024);
        assert_eq!(p.system_prompt_budget, 1_500);
        assert_eq!(p.usable_prompt_tokens(), 256);
        assert_eq!(p.usable_history_tokens(), 0);
    }
}
