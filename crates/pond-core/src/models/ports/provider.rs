use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use thiserror::Error;

pub use crate::models::domain::message::ChatMessage;
use crate::models::domain::model_capabilities::ModelCapabilities;

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Provider error: {0}")]
    General(String),

    #[error("Model not available: {0}")]
    ModelNotAvailable(String),
}

/// Token usage reported by the model after a completion.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct UsageStats {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// Tokens the model spent on reasoning it did not say out loud. GIAP-derived, not
    /// provider-reported: counted through the
    /// [`TokenCounter`](crate::models::ports::token_counter::TokenCounter) port, so never exact.
    /// Not subtracted from `completion_tokens`, and `None` means nobody counted, not zero.
    pub reasoning_tokens: Option<u32>,
}

/// A single item emitted by [`TokenStream`].
#[derive(Debug, Clone)]
pub enum StreamToken {
    /// A text fragment (token) produced by the model.
    Text(String),
    /// Final token-usage statistics — emitted once as the **last** stream item
    /// by providers that support usage tracking (llamafile, Ollama).
    /// Consumers should skip this when building the response text.
    Usage(UsageStats),
}

/// A pinned, boxed stream of [`StreamToken`] items.
/// Lifetime `'a` is tied to the provider reference so borrowing-based impls work.
pub type TokenStream<'a> = Pin<Box<dyn Stream<Item = Result<StreamToken>> + Send + 'a>>;

/// Driven Port: LlmProvider
///
/// Abstracts the LLM backend so the domain can request completions
/// without knowing whether it's OpenAI, Ollama, llama.cpp, or a mock.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a conversation and get a completion back.
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage>;

    /// The name of the underlying model (e.g. "llama-3.2-3b", "gpt-4o").
    fn model_name(&self) -> String;

    /// Runtime capabilities of the underlying model. Providers override this to declare what the
    /// active model supports (thinking, vision, context window); the default is the most
    /// conservative assumption, so an unknown model still works safely.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    /// Stream tokens as they are generated. The default calls `complete()` and yields the whole
    /// text as one [`StreamToken::Text`]; adapters with native streaming override this and may
    /// append a final [`StreamToken::Usage`].
    fn stream_complete<'a>(
        &'a self,
        system_prompt: &'a str,
        messages: Vec<ChatMessage>,
    ) -> TokenStream<'a> {
        Box::pin(async_stream::stream! {
            match self.complete(system_prompt, messages).await {
                Ok(msg)  => yield Ok(StreamToken::Text(msg.content.clone())),
                Err(e)   => yield Err(e),
            }
        })
    }
}

/// `complete()` always fails with a clear message — the fallback when a
/// requested provider is selected but isn't actually available. Not a
/// silent fallback to a working provider: the user should know their
/// choice didn't take effect, not get a different model's answer instead.
pub struct UnavailableProvider {
    message: String,
}

impl UnavailableProvider {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait]
impl LlmProvider for UnavailableProvider {
    async fn complete(
        &self,
        _system_prompt: &str,
        _messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        Err(anyhow::anyhow!(self.message.clone()))
    }

    fn model_name(&self) -> String {
        "mesh (unavailable)".to_string()
    }
}
