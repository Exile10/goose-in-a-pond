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
    /// Tokens the model spent on reasoning it did not say out loud.
    ///
    /// **GIAP-derived, not provider-reported.** No provider GIAP ships reports
    /// this: Goose's own `Usage` carries input/output/total/cache_read/
    /// cache_write and nothing else, and the OpenAI-shaped HTTP providers put
    /// reasoning in a separate *content* channel rather than a separate counter.
    /// So this is GIAP counting the reasoning text it received, through the
    /// [`TokenCounter`](crate::models::ports::token_counter::TokenCounter) port
    /// — which reports `is_exact() == false` for every implementation. Treat it
    /// as a measurement, never as ground truth.
    ///
    /// **Not subtracted from `completion_tokens`.** The provider's output count
    /// most likely already includes the reasoning decode, but nobody has
    /// measured which way for the models GIAP pins, and guessing would corrupt
    /// the one number that *is* provider-reported. This rides alongside.
    ///
    /// `None` means nobody counted, which is different from `Some(0)` — "this
    /// turn produced no reasoning". Anything deriving a budget from this (PAI-5
    /// P5's `output_reserve_tokens`) must keep the two apart.
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

    /// Runtime capabilities of the underlying model.
    ///
    /// Providers override this to declare what the active model supports
    /// (thinking, vision, context window, etc.). The default returns the
    /// most conservative assumptions so unknown models work safely.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    /// Stream tokens as they are generated.
    ///
    /// The default implementation calls `complete()` and yields the full text as a single
    /// [`StreamToken::Text`] chunk.  Adapters that support native streaming override this
    /// to yield tokens as they arrive and optionally append a final [`StreamToken::Usage`].
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
