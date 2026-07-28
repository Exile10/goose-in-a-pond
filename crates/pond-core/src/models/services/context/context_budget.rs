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
}

impl CompactionProfile {
    /// Tokens the prompt may occupy: the window minus the output reserve.
    pub fn usable_prompt_tokens(&self) -> usize {
        self.context_window_tokens
            .saturating_sub(self.output_reserve_tokens)
    }
}

impl CompactionProfile {
    /// Derive a compaction profile from the effective context window in tokens.
    ///
    /// The tiers are tuned for GIAP's chat workflow:
    /// - **3K** (Jetson CUDA): aggressive compaction, minimal memory injection.
    /// - **8K** (macOS Metal default): balanced — enough for 5 memories + prompt.
    /// - **32K** (Ollama/llamafile with medium models): generous budgets.
    /// - **128K+** (large context HTTP models): near-unlimited for local use.
    pub fn from_context_window(context_tokens: usize) -> Self {
        if context_tokens <= 4096 {
            // Jetson-class: 3K–4K tokens
            Self {
                compaction_threshold: 0.60,
                memory_token_budget: 200,
                max_memory_fragments: 3,
                system_prompt_budget: 1500,
                history_token_budget: 1200,
                // A thinking block alone measured 306 tokens on this class of
                // device; 768 covers reasoning plus a real answer.
                output_reserve_tokens: 768,
                context_window_tokens: context_tokens,
            }
        } else if context_tokens <= 12288 {
            // macOS Metal default: 8K–12K tokens
            Self {
                compaction_threshold: 0.70,
                memory_token_budget: 500,
                max_memory_fragments: 5,
                system_prompt_budget: 3000,
                history_token_budget: 4000,
                output_reserve_tokens: 1024,
                context_window_tokens: context_tokens,
            }
        } else if context_tokens <= 65536 {
            // Medium context: 32K–64K tokens
            Self {
                compaction_threshold: 0.75,
                memory_token_budget: 1500,
                max_memory_fragments: 10,
                system_prompt_budget: 6000,
                history_token_budget: 20000,
                output_reserve_tokens: 2048,
                context_window_tokens: context_tokens,
            }
        } else {
            // Large context: 128K+ tokens
            Self {
                compaction_threshold: 0.80,
                memory_token_budget: 4000,
                max_memory_fragments: 15,
                system_prompt_budget: 10000,
                history_token_budget: 80000,
                output_reserve_tokens: 4096,
                context_window_tokens: context_tokens,
            }
        }
    }

    /// Whether the system prompt should use a compact format.
    ///
    /// Returns true when the context window is small enough that verbose
    /// tool descriptions and detailed instructions waste precious tokens.
    pub fn use_compact_prompt(&self) -> bool {
        self.context_window_tokens <= 12288
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
}
