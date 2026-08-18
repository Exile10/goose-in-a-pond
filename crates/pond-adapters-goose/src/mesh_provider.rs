//! Bridges pond-core's `LlmProvider` port to Goose's native `Provider` trait
//! (#132 Milestone 4), so `chat_provider = "mesh"` can drive real chat
//! through `GooseAdapter` — not just the `GET /api/v1/test` diagnostic probe
//! `pond-adapters-mesh-inference` was wired into first.
//!
//! Deliberately the *inverse* direction of `provider_adapter.rs`'s
//! `GooseProviderAdapter` (which wraps a Goose `Provider` as an
//! `LlmProvider`) — this wraps an `LlmProvider` as a Goose `Provider`.
//!
//! **No MCP tool-calling over mesh, by construction, not by oversight.**
//! Goose bakes `tools: &[Tool]` into every `stream()` call and expects
//! tool-call requests back in the response; `LlmProvider` has no tools
//! parameter at all (confirmed by `GooseProviderAdapter::complete`, which
//! already hardcodes `&[]` for the same reason). Extending `LlmProvider`
//! itself would ripple into every other adapter (ollama, llamafile, local) —
//! real scope, not this milestone. `tools` is silently dropped here at
//! debug level, not warned — GIAP's `giap-draft` extension is always-on, so
//! tools are present on nearly every real turn, and a per-request warning
//! for expected, by-design behavior would just be noise.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use goose::conversation::message::Message;
use goose::providers::base::{MessageStream, Provider, ProviderUsage, Usage};
use goose_providers::errors::ProviderError;
use goose_providers::model::ModelConfig;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::ports::provider::{LlmProvider, StreamToken};
use rmcp::model::Tool;

pub struct MeshProvider {
    inner: Arc<dyn LlmProvider>,
}

impl MeshProvider {
    pub fn new(inner: Arc<dyn LlmProvider>) -> Self {
        Self { inner }
    }

    /// Goose has no `System`/`Tool` message role at the type level — mirrors
    /// `GooseProviderAdapter::from_goose_message`'s existing treatment
    /// exactly (`provider_adapter.rs`), just run in the opposite direction.
    fn from_goose_message(msg: &Message) -> ChatMessage {
        let role = match msg.role {
            rmcp::model::Role::User => Role::User,
            rmcp::model::Role::Assistant => Role::Assistant,
        };
        ChatMessage {
            role,
            content: msg.as_concat_text(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

#[async_trait]
impl Provider for MeshProvider {
    fn get_name(&self) -> &str {
        "mesh"
    }

    async fn stream(
        &self,
        _model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        if !tools.is_empty() {
            tracing::debug!(
                tool_count = tools.len(),
                "mesh provider: dropping tools — MCP tool-calling is not available over the mesh yet"
            );
        }

        let chat_messages: Vec<ChatMessage> =
            messages.iter().map(Self::from_goose_message).collect();
        let system = system.to_string();
        // Owned clones moved into the generator below so the returned stream
        // is 'static (Goose's `MessageStream` alias carries no lifetime) —
        // `self.inner.stream_complete(...)` itself returns a stream borrowing
        // `&self`, which would not outlive this method call otherwise.
        let inner = self.inner.clone();

        let stream = async_stream::stream! {
            let mut token_stream = inner.stream_complete(&system, chat_messages);
            while let Some(item) = token_stream.next().await {
                match item {
                    Ok(StreamToken::Text(text)) => {
                        yield Ok((Some(Message::assistant().with_text(&text)), None));
                    }
                    Ok(StreamToken::Usage(stats)) => {
                        let usage = Usage {
                            input_tokens: Some(stats.prompt_tokens as i32),
                            output_tokens: Some(stats.completion_tokens as i32),
                            total_tokens: Some((stats.prompt_tokens + stats.completion_tokens) as i32),
                            ..Default::default()
                        };
                        yield Ok((None, Some(ProviderUsage::new("mesh".to_string(), usage))));
                    }
                    Err(err) => {
                        yield Err(ProviderError::ExecutionError(err.to_string()));
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::mocks::mock_provider::MockProvider;

    fn user_message(text: &str) -> Message {
        Message::user().with_text(text)
    }

    #[test]
    fn from_goose_message_translates_user_and_assistant() {
        let user = Message::user().with_text("hello");
        let translated = MeshProvider::from_goose_message(&user);
        assert_eq!(translated.role, Role::User);
        assert_eq!(translated.content, "hello");

        let assistant = Message::assistant().with_text("hi there");
        let translated = MeshProvider::from_goose_message(&assistant);
        assert_eq!(translated.role, Role::Assistant);
        assert_eq!(translated.content, "hi there");
    }

    /// `MockProvider`'s `stream_complete` uses the `LlmProvider` trait's
    /// default (text-only, never yields `StreamToken::Usage`), so it can't
    /// exercise the usage-translation path — this stub emits both, mirroring
    /// what a real streaming provider (or `MeshInferenceProvider`'s own
    /// responder) actually sends.
    struct StreamingStubProvider;

    #[async_trait]
    impl LlmProvider for StreamingStubProvider {
        async fn complete(
            &self,
            _system_prompt: &str,
            _messages: Vec<ChatMessage>,
        ) -> anyhow::Result<ChatMessage> {
            Ok(ChatMessage::assistant("hi there"))
        }

        fn model_name(&self) -> String {
            "stub".to_string()
        }

        fn stream_complete<'a>(
            &'a self,
            _system_prompt: &'a str,
            _messages: Vec<ChatMessage>,
        ) -> pond_core::models::ports::provider::TokenStream<'a> {
            Box::pin(async_stream::stream! {
                yield Ok(StreamToken::Text("hi ".to_string()));
                yield Ok(StreamToken::Text("there".to_string()));
                yield Ok(StreamToken::Usage(pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    reasoning_tokens: None,
                }));
            })
        }
    }

    #[tokio::test]
    async fn stream_yields_text_deltas_then_usage() {
        let provider = MeshProvider::new(Arc::new(StreamingStubProvider));
        let cfg = ModelConfig::new("mesh");
        let mut stream = provider
            .stream(&cfg, "You are helpful.", &[user_message("hi")], &[])
            .await
            .unwrap();

        let mut texts = Vec::new();
        let mut saw_usage = false;
        while let Some(item) = stream.next().await {
            let (message, usage) = item.unwrap();
            if let Some(msg) = message {
                texts.push(msg.as_concat_text());
            }
            if let Some(usage) = usage {
                assert_eq!(usage.usage.input_tokens, Some(3));
                assert_eq!(usage.usage.output_tokens, Some(2));
                saw_usage = true;
            }
        }
        // Each StreamToken::Text is its own delta, in order — not accumulated.
        assert_eq!(texts, vec!["hi ".to_string(), "there".to_string()]);
        assert!(saw_usage, "expected a terminal usage item");
    }

    #[tokio::test]
    async fn stream_drops_tools_without_erroring() {
        let provider = MeshProvider::new(Arc::new(MockProvider::new()));
        let cfg = ModelConfig::new("mesh");
        let tool = Tool::new("device_control", "control a device", serde_json::Map::new());
        let result = provider
            .stream(
                &cfg,
                "sys",
                &[user_message("hi")],
                std::slice::from_ref(&tool),
            )
            .await;
        assert!(result.is_ok(), "a non-empty tools list must not error");
    }

    #[tokio::test]
    async fn stream_propagates_inner_errors() {
        struct FailingProvider;
        #[async_trait]
        impl LlmProvider for FailingProvider {
            async fn complete(
                &self,
                _system_prompt: &str,
                _messages: Vec<ChatMessage>,
            ) -> anyhow::Result<ChatMessage> {
                Err(anyhow::anyhow!("no peer available"))
            }
            fn model_name(&self) -> String {
                "failing".to_string()
            }
        }

        let provider = MeshProvider::new(Arc::new(FailingProvider));
        let cfg = ModelConfig::new("mesh");
        let mut stream = provider
            .stream(&cfg, "sys", &[user_message("hi")], &[])
            .await
            .unwrap();

        let first = stream.next().await.expect("expected one item");
        assert!(matches!(first, Err(ProviderError::ExecutionError(_))));
    }
}
