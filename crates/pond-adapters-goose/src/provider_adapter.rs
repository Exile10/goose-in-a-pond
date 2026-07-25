use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose::conversation::message::Message as GooseMessage;
use goose::providers::base::Provider as GooseProvider;
use goose_providers::model::ModelConfig;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::ports::provider::LlmProvider;
use std::sync::Arc;

/// Adapter: GooseProviderAdapter
///
/// Bridges pond's `LlmProvider` port to Goose's `Provider` trait.
/// Converts between pond `ChatMessage` and Goose `Message` types.
/// Goose providers are model-agnostic; the `ModelConfig` selects the model
/// per call, so this adapter carries it alongside the provider.
pub struct GooseProviderAdapter {
    provider: Arc<dyn GooseProvider>,
    model_config: ModelConfig,
}

impl GooseProviderAdapter {
    pub fn new(provider: Arc<dyn GooseProvider>, model_config: ModelConfig) -> Self {
        Self {
            provider,
            model_config,
        }
    }

    /// Convert a pond ChatMessage into a Goose Message.
    fn to_goose_message(msg: &ChatMessage) -> GooseMessage {
        match msg.role {
            Role::User => GooseMessage::user().with_text(&msg.content),
            Role::Assistant => GooseMessage::assistant().with_text(&msg.content),
            Role::System => {
                // Goose doesn't have a System message role —
                // system content is passed as the `system` parameter to `complete()`.
                // We treat System messages as User messages with a note.
                GooseMessage::user().with_text(&msg.content)
            }
            Role::Tool => {
                // Tool results are treated as user messages in the Goose provider adapter.
                GooseMessage::user().with_text(&msg.content)
            }
        }
    }

    /// Convert a Goose Message back into a pond ChatMessage.
    fn from_goose_message(msg: &GooseMessage) -> ChatMessage {
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
impl LlmProvider for GooseProviderAdapter {
    fn capabilities(&self) -> pond_core::models::domain::model_capabilities::ModelCapabilities {
        pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
            &self.model_config.model_name,
        )
    }

    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        // Convert pond messages to Goose messages, filtering out System role
        // (system content goes into the `system` parameter instead)
        let goose_messages: Vec<GooseMessage> = messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(Self::to_goose_message)
            .collect();

        let (response, _usage) = self
            .provider
            .complete(
                &self.model_config,
                system_prompt,
                &goose_messages,
                &[], // no tools for direct completion
            )
            .await
            .map_err(|e| anyhow!("Goose provider error: {}", e))?;

        Ok(Self::from_goose_message(&response))
    }

    fn model_name(&self) -> String {
        self.model_config.model_name.clone()
    }
}
