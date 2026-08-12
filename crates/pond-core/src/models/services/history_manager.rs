//! Conversation history assembly for context injection.
//!
//! The [`HistoryManager`] groups stored session messages into atomic *turns*
//! (a user message plus all subsequent assistant/tool messages until the next
//! user message), then walks newest-first to keep turns that fit within a
//! character budget. Turns are never split — a tool call and its result must
//! travel together so the model never sees an orphan tool result.
//!
//! Output is always in chronological (oldest-first) order so it can be
//! consumed directly by `InferenceProvider::stream_chat`.

use crate::models::domain::message::{ChatMessage, Role};
use crate::user_data::domain::session::SessionMessage;

/// Builds conversation history for context injection.
pub struct HistoryManager {
    history_char_budget: usize,
}

impl HistoryManager {
    pub fn new(history_char_budget: usize) -> Self {
        Self {
            history_char_budget,
        }
    }

    /// Build the history message array from stored session messages.
    ///
    /// Returns messages in chronological order, trimmed to fit the budget.
    pub fn build_history(&self, stored: &[SessionMessage]) -> Vec<ChatMessage> {
        if stored.is_empty() {
            return Vec::new();
        }

        let messages: Vec<ChatMessage> = stored.iter().map(|s| s.message.clone()).collect();

        // Group into turns starting at each user message.
        let turns = group_into_turns(&messages);

        // Walk newest-first, keep turns that fit in budget.
        let mut kept: Vec<&[ChatMessage]> = Vec::new();
        let mut used_chars = 0usize;

        for turn in turns.iter().rev() {
            let turn_chars: usize = turn.iter().map(|m| m.content.len()).sum();
            if used_chars + turn_chars > self.history_char_budget && !kept.is_empty() {
                break;
            }
            used_chars += turn_chars;
            kept.push(turn);
        }

        // Reverse to chronological order and flatten.
        kept.reverse();
        kept.into_iter().flat_map(|t| t.iter().cloned()).collect()
    }
}

/// Group a flat message list into turns.
///
/// Each turn starts with a user message and includes all subsequent
/// non-user messages until the next user message. Stray leading
/// non-user messages (e.g. an orphan tool result with no preceding user
/// turn) are dropped — they'd confuse the model.
fn group_into_turns(messages: &[ChatMessage]) -> Vec<&[ChatMessage]> {
    let mut turns: Vec<&[ChatMessage]> = Vec::new();
    let mut start = 0;

    for i in 1..=messages.len() {
        let at_end = i == messages.len();
        let next_is_user = !at_end && messages[i].role == Role::User;

        if (next_is_user || at_end) && start < i {
            // Only include turns that start with a user message.
            if messages[start].role == Role::User {
                turns.push(&messages[start..i]);
            }
            start = i;
        }
    }

    turns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::message::{ChatMessage, Role, ToolCallRecord};
    use crate::user_data::domain::session::SessionMessage;

    fn session_msg(role: Role, content: &str) -> SessionMessage {
        SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "test".to_string(),
            message: ChatMessage {
                role,
                content: content.to_string(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            created_at: chrono::Utc::now(),
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            liked: None,
        }
    }

    #[test]
    fn empty_history_returns_empty() {
        let mgr = HistoryManager::new(10000);
        assert!(mgr.build_history(&[]).is_empty());
    }

    #[test]
    fn single_turn_passes_through() {
        let stored = vec![
            session_msg(Role::User, "hello"),
            session_msg(Role::Assistant, "hi there"),
        ];
        let result = HistoryManager::new(10000).build_history(&stored);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hello");
        assert_eq!(result[1].content, "hi there");
    }

    #[test]
    fn over_budget_drops_oldest_turn() {
        // Two turns, budget only fits one.
        let stored = vec![
            session_msg(Role::User, &"a".repeat(500)),
            session_msg(Role::Assistant, &"b".repeat(500)),
            session_msg(Role::User, &"c".repeat(500)),
            session_msg(Role::Assistant, &"d".repeat(500)),
        ];
        let result = HistoryManager::new(1001).build_history(&stored);
        // Should keep only the most recent turn (1001 chars > 500+500=1000)
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "c".repeat(500));
    }

    #[test]
    fn tool_calls_kept_with_turn() {
        // Turn with tool call must not be split.
        let stored = vec![
            session_msg(Role::User, "weather?"),
            session_msg(Role::Assistant, ""),
            session_msg(Role::Tool, "sunny"),
            session_msg(Role::Assistant, "It's sunny"),
        ];
        let result = HistoryManager::new(10000).build_history(&stored);
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn chronological_order_preserved() {
        let stored: Vec<SessionMessage> = (0..6)
            .map(|i| {
                if i % 2 == 0 {
                    session_msg(Role::User, &format!("user-{}", i))
                } else {
                    session_msg(Role::Assistant, &format!("asst-{}", i))
                }
            })
            .collect();
        let result = HistoryManager::new(10000).build_history(&stored);
        assert_eq!(result[0].content, "user-0");
        assert_eq!(result[5].content, "asst-5");
    }

    #[test]
    fn tool_call_metadata_preserved_through_history() {
        // Assistant turn with tool call metadata must keep its tool_calls field.
        let asst_with_call = SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "test".to_string(),
            message: ChatMessage::assistant_with_tool_calls(
                "calling weather",
                vec![ToolCallRecord {
                    id: "call-1".to_string(),
                    name: "get_weather".to_string(),
                    arguments: "{}".to_string(),
                }],
            ),
            created_at: chrono::Utc::now(),
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            liked: None,
        };
        let tool_result = SessionMessage {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "test".to_string(),
            message: ChatMessage::tool_result("sunny", "call-1"),
            created_at: chrono::Utc::now(),
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            liked: None,
        };

        let stored = vec![
            session_msg(Role::User, "weather?"),
            asst_with_call,
            tool_result,
        ];
        let result = HistoryManager::new(10000).build_history(&stored);
        assert_eq!(result.len(), 3);
        assert_eq!(result[1].tool_calls.len(), 1);
        assert_eq!(result[1].tool_calls[0].id, "call-1");
        assert_eq!(result[2].tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn leading_orphan_tool_message_is_dropped() {
        // History that starts with a tool result with no preceding user turn
        // should drop the orphan.
        let stored = vec![
            session_msg(Role::Tool, "orphan result"),
            session_msg(Role::User, "hi"),
            session_msg(Role::Assistant, "hello"),
        ];
        let result = HistoryManager::new(10000).build_history(&stored);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hi");
    }
}
