//! Context budget management for constrained LLM inference.
//!
//! On Jetson Orin Nano with a 7B Q4 model, the context window is ~8K tokens.
//! After reserving space for the system prompt and the generated response,
//! roughly 2,500 tokens (~9,952 chars at 4 chars/token) are usable for history.
//!
//! `trim_to_budget()` walks messages newest-first and keeps messages until
//! the character budget is exhausted, then reverses to restore chronological order.
//! This ensures the most recent context is always preserved.

use crate::domain::message::ChatMessage;

/// Approximate total context window in characters (8K tokens × 4 chars/token).
pub const MAX_CONTEXT_CHARS: usize = 12_000;

/// Characters reserved for the LLM's generated response.
pub const RESERVE_FOR_RESPONSE_CHARS: usize = 2_048;

/// Characters available for conversation history after reserving for response.
pub const USABLE_HISTORY_CHARS: usize = MAX_CONTEXT_CHARS - RESERVE_FOR_RESPONSE_CHARS;

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
    let mut kept: Vec<ChatMessage> = Vec::new();
    let mut remaining = USABLE_HISTORY_CHARS;

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
            let truncated = msg.content[..remaining].to_string();
            kept.push(ChatMessage { content: truncated, ..msg });
            break;
        } else {
            // Later messages don't fit — stop here.
            break;
        }
    }

    kept.reverse();
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::message::Role;

    fn msg(content: &str) -> ChatMessage {
        ChatMessage { role: Role::User, content: content.to_string() }
    }

    fn total_chars(msgs: &[ChatMessage]) -> usize {
        msgs.iter().map(|m| m.content.len()).sum()
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(trim_to_budget(vec![]).is_empty());
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
        let messages: Vec<ChatMessage> =
            (0..200).map(|_| msg(&"x".repeat(200))).collect();
        let result = trim_to_budget(messages);
        assert!(total_chars(&result) <= USABLE_HISTORY_CHARS);
        // Should keep at least 1 message
        assert!(!result.is_empty());
    }

    #[test]
    fn most_recent_messages_are_preserved() {
        // Fill budget with old junk, then add a recent message that fits
        let mut messages: Vec<ChatMessage> =
            (0..60).map(|_| msg(&"a".repeat(200))).collect();
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
            let a: u32 = w[0].content.strip_prefix("message-").unwrap().parse().unwrap();
            let b: u32 = w[1].content.strip_prefix("message-").unwrap().parse().unwrap();
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
}
