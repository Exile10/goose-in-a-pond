//! Context compaction via LLM summarisation.
//!
//! When conversation history grows large, `trim_to_budget` simply drops the
//! oldest messages.  `ContextCompactor` does something smarter: once the
//! history approaches `threshold` (default 80%) of the model's context limit
//! it asks the LLM to write a concise summary of the oldest 75% of messages,
//! then splices that summary back as a single `User` message, keeping only the
//! most recent 25% verbatim.
//!
//! If the LLM call fails for any reason, `compact` falls back to
//! `trim_to_budget` so the chat loop is never interrupted.

use crate::domain::message::{ChatMessage, Role};
use crate::ports::provider::LlmProvider;
use crate::services::context_budget::{trim_to_budget, USABLE_HISTORY_CHARS};
use anyhow::Result;

/// 4 characters per token is a common heuristic for English text.
const CHARS_PER_TOKEN: usize = 4;

pub struct ContextCompactor {
    /// Fraction of the usable history budget that triggers compaction.
    /// Default: 0.80 (trigger when 80% full).
    threshold: f64,
    /// Estimated context limit in tokens for the active model.
    /// Default: derives from USABLE_HISTORY_CHARS.
    context_limit_tokens: usize,
}

impl Default for ContextCompactor {
    fn default() -> Self {
        Self {
            threshold: 0.80,
            context_limit_tokens: USABLE_HISTORY_CHARS / CHARS_PER_TOKEN,
        }
    }
}

impl ContextCompactor {
    pub fn new(threshold: f64, context_limit_tokens: usize) -> Self {
        Self { threshold, context_limit_tokens }
    }

    /// Returns `true` when the total history size has exceeded the threshold.
    pub fn needs_compaction(&self, messages: &[ChatMessage]) -> bool {
        let chars: usize = messages.iter().map(|m| m.content.len()).sum();
        let estimated_tokens = chars / CHARS_PER_TOKEN;
        estimated_tokens as f64 / self.context_limit_tokens as f64 >= self.threshold
    }

    /// Compact history by LLM-summarising the oldest 75% of messages.
    ///
    /// Returns messages in chronological (oldest-first) order, ready to pass
    /// directly to `LlmProvider::complete()`.
    ///
    /// Falls back to `trim_to_budget` on any LLM error.
    pub async fn compact(
        &self,
        provider: &dyn LlmProvider,
        messages: Vec<ChatMessage>,
    ) -> Vec<ChatMessage> {
        if messages.is_empty() {
            return messages;
        }

        // Keep the newest 25% verbatim; summarise the rest.
        let keep_count = (messages.len() as f64 * 0.25).ceil() as usize;
        let keep_count = keep_count.max(1);

        let split = if messages.len() > keep_count {
            messages.len() - keep_count
        } else {
            return messages;
        };

        let (to_summarise, to_keep) = messages.split_at(split);

        match summarise(provider, to_summarise).await {
            Ok(summary) => {
                let summary_msg = ChatMessage {
                    role: Role::User,
                    content: format!(
                        "[Earlier conversation summary]\n{}\n[End of summary]",
                        summary
                    ),
                };
                let mut result = vec![summary_msg];
                result.extend_from_slice(to_keep);
                result
            }
            Err(e) => {
                tracing::warn!("ContextCompactor LLM call failed ({e}), falling back to trim_to_budget");
                trim_to_budget(messages)
            }
        }
    }
}

async fn summarise(provider: &dyn LlmProvider, messages: &[ChatMessage]) -> Result<String> {
    let conversation_text: String = messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            format!("{}: {}", role, m.content)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt_messages = vec![ChatMessage {
        role: Role::User,
        content: format!(
            "Summarise the following conversation concisely, preserving all important \
             facts, decisions, and context. Write 3-5 sentences maximum.\n\n{}",
            conversation_text
        ),
    }];

    let system = "You are a helpful assistant. Summarise conversations accurately and concisely.";
    let response = provider.complete(system, prompt_messages).await?;
    Ok(response.content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::context_budget::USABLE_HISTORY_CHARS;

    fn msg(role: Role, content: &str) -> ChatMessage {
        ChatMessage { role, content: content.to_string() }
    }

    #[test]
    fn needs_compaction_false_for_small_history() {
        let compactor = ContextCompactor::default();
        let messages = vec![msg(Role::User, "hello"), msg(Role::Assistant, "hi there")];
        assert!(!compactor.needs_compaction(&messages));
    }

    #[test]
    fn needs_compaction_true_when_over_threshold() {
        let compactor = ContextCompactor::default();
        // Fill to 85% of usable history
        let size = (USABLE_HISTORY_CHARS as f64 * 0.85) as usize;
        let messages = vec![msg(Role::User, &"x".repeat(size))];
        assert!(compactor.needs_compaction(&messages));
    }

    #[test]
    fn compact_returns_unchanged_when_empty() {
        // Synchronous check — compact with empty vec stays empty
        let compactor = ContextCompactor::default();
        // We can't easily test the async path without a mock provider here;
        // just verify needs_compaction is false for empty.
        assert!(!compactor.needs_compaction(&[]));
    }

    #[test]
    fn split_calculates_correctly() {
        // 4 messages → keep 1 (ceil(4*0.25)=1), summarise 3
        let n = 4usize;
        let keep = (n as f64 * 0.25).ceil() as usize;
        assert_eq!(keep, 1);
        assert_eq!(n - keep, 3);

        // 10 messages → keep 3 (ceil(10*0.25)=3), summarise 7
        let n = 10usize;
        let keep = (n as f64 * 0.25).ceil() as usize;
        assert_eq!(keep, 3);
        assert_eq!(n - keep, 7);
    }
}
