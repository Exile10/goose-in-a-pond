//! ModelRouter — an `LlmProvider` that dispatches to role-specific providers.
//!
//! Sits between `ChatService` and the concrete LLM providers. All requests
//! currently route to the `chat` provider. The `think` and `task` providers
//! are retained for future per-role model assignment but are not auto-selected.
//!
//! If `think` or `task` arcs are the same object as `chat`, no extra latency
//! is incurred — it's just a pointer comparison that short-circuits.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tracing::debug;

use crate::models::domain::message::ChatMessage;
use crate::models::domain::model_role::ModelRole;
use crate::models::ports::provider::LlmProvider;

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
        // All requests route to the chat provider. The LLM decides tool use
        // natively via MCP — no pre-classification needed.
        let role = ModelRole::Chat;

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

    fn stream_complete<'a>(
        &'a self,
        system_prompt: &'a str,
        messages: Vec<ChatMessage>,
    ) -> crate::models::ports::provider::TokenStream<'a> {
        // All requests route to the chat provider.
        let role = ModelRole::Chat;
        self.provider_for(role).stream_complete(system_prompt, messages)
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
    async fn stream_complete_delegates_to_correct_provider() {
        use futures::StreamExt;

        let chat  = Arc::new(CountingMock::new("chat-model"));
        let think = Arc::new(CountingMock::new("think-model"));
        let task  = Arc::new(CountingMock::new("task-model"));
        let router = ModelRouter::new(chat.clone(), think.clone(), task);

        let messages = vec![ChatMessage::user("hello there")];
        let mut stream = router.stream_complete("sys", messages);
        let first = stream.next().await;
        assert!(first.is_some());
        assert!(first.unwrap().is_ok());
        // stream_complete should route "hello there" to chat provider
        assert_eq!(chat.call_count(), 1);
        assert_eq!(think.call_count(), 0);
    }

    #[tokio::test]
    async fn stream_complete_routes_think_to_think_provider() {
        use futures::StreamExt;

        let chat  = Arc::new(CountingMock::new("chat-model"));
        let think = Arc::new(CountingMock::new("think-model"));
        let task  = Arc::new(CountingMock::new("task-model"));
        let router = ModelRouter::new(chat.clone(), think.clone(), task);

        let messages = vec![ChatMessage::user("explain why the sky is blue")];
        let mut stream = router.stream_complete("sys", messages);
        let _ = stream.next().await;
        assert_eq!(think.call_count(), 1);
        assert_eq!(chat.call_count(), 0);
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
