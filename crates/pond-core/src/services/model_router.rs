//! ModelRouter — an `LlmProvider` that dispatches to role-specific providers.
//!
//! Sits between `ChatService` and the concrete LLM providers. On every request
//! it:
//! 1. Extracts the last user message.
//! 2. Calls `classify_request()` to determine the `ModelRole`.
//! 3. Forwards to the appropriate provider arc.
//!
//! If `think` or `task` arcs are the same object as `chat`, no extra latency
//! is incurred — it's just a pointer comparison that short-circuits.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tracing::debug;

use crate::domain::message::{ChatMessage, Role};
use crate::domain::model_role::ModelRole;
use crate::ports::provider::LlmProvider;
use crate::services::request_classifier::classify_request;

pub struct ModelRouter {
    chat:  Arc<dyn LlmProvider>,
    think: Arc<dyn LlmProvider>,
    task:  Arc<dyn LlmProvider>,
}

impl ModelRouter {
    /// Create a router.
    ///
    /// Pass the same `Arc` for `think` / `task` when no distinct model is
    /// configured — they will be treated as aliases for `chat`.
    pub fn new(
        chat:  Arc<dyn LlmProvider>,
        think: Arc<dyn LlmProvider>,
        task:  Arc<dyn LlmProvider>,
    ) -> Self {
        Self { chat, think, task }
    }

    fn provider_for(&self, role: ModelRole) -> &Arc<dyn LlmProvider> {
        match role {
            ModelRole::Chat  => &self.chat,
            ModelRole::Think => &self.think,
            ModelRole::Task  => &self.task,
        }
    }
}

#[async_trait]
impl LlmProvider for ModelRouter {
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        // Extract the last user message text for classification.
        let last_user_text = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.as_str())
            .unwrap_or("");

        let role = classify_request(last_user_text);

        debug!(
            role = ?role,
            model = %self.provider_for(role).model_name(),
            "ModelRouter dispatching"
        );

        self.provider_for(role).complete(system_prompt, messages).await
    }

    fn model_name(&self) -> String {
        format!(
            "router(chat={}, think={}, task={})",
            self.chat.model_name(),
            self.think.model_name(),
            self.task.model_name(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counting mock that records which model handled each call.
    struct CountingMock {
        name:  String,
        calls: AtomicUsize,
    }

    impl CountingMock {
        fn new(name: &str) -> Self {
            Self { name: name.to_string(), calls: AtomicUsize::new(0) }
        }
        fn call_count(&self) -> usize { self.calls.load(Ordering::SeqCst) }
    }

    #[async_trait]
    impl LlmProvider for CountingMock {
        async fn complete(&self, _sys: &str, _msgs: Vec<ChatMessage>) -> Result<ChatMessage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ChatMessage::assistant(format!("{} response", self.name)))
        }
        fn model_name(&self) -> String { self.name.clone() }
    }

    #[tokio::test]
    async fn routes_chat_message_to_chat_provider() {
        let chat  = Arc::new(CountingMock::new("chat-model"));
        let think = Arc::new(CountingMock::new("think-model"));
        let task  = Arc::new(CountingMock::new("task-model"));
        let router = ModelRouter::new(chat.clone(), think, task);

        router.complete("sys", vec![ChatMessage::user("hello there")]).await.unwrap();

        assert_eq!(chat.call_count(), 1);
    }

    #[tokio::test]
    async fn routes_think_message_to_think_provider() {
        let chat  = Arc::new(CountingMock::new("chat-model"));
        let think = Arc::new(CountingMock::new("think-model"));
        let task  = Arc::new(CountingMock::new("task-model"));
        let router = ModelRouter::new(chat, think.clone(), task);

        router.complete("sys", vec![ChatMessage::user("explain why water boils")]).await.unwrap();

        assert_eq!(think.call_count(), 1);
    }

    #[tokio::test]
    async fn routes_task_message_to_task_provider() {
        let chat  = Arc::new(CountingMock::new("chat-model"));
        let think = Arc::new(CountingMock::new("think-model"));
        let task  = Arc::new(CountingMock::new("task-model"));
        let router = ModelRouter::new(chat, think, task.clone());

        router.complete("sys", vec![ChatMessage::user("remind me at 9am")]).await.unwrap();

        assert_eq!(task.call_count(), 1);
    }

    #[tokio::test]
    async fn model_name_includes_all_three() {
        let chat  = Arc::new(CountingMock::new("chat-model"));
        let think = Arc::new(CountingMock::new("think-model"));
        let task  = Arc::new(CountingMock::new("task-model"));
        let router = ModelRouter::new(chat, think, task);

        let name = router.model_name();
        assert!(name.contains("chat-model"));
        assert!(name.contains("think-model"));
        assert!(name.contains("task-model"));
    }
}
