use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use thiserror::Error;

pub use crate::domain::message::ChatMessage;

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Provider error: {0}")]
    General(String),

    #[error("Model not available: {0}")]
    ModelNotAvailable(String),
}

/// A pinned, boxed stream of tokens.  Each item is either a token string or an error.
/// Lifetime `'a` is tied to the provider reference so borrowing-based impls work.
pub type TokenStream<'a> = Pin<Box<dyn Stream<Item = Result<String>> + Send + 'a>>;

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

    /// Stream tokens as they are generated.
    ///
    /// The default implementation calls `complete()` and yields the full text as a single
    /// chunk.  Adapters that support native streaming should override this method to stream
    /// tokens as they arrive from the backend.
    fn stream_complete<'a>(
        &'a self,
        system_prompt: &'a str,
        messages: Vec<ChatMessage>,
    ) -> TokenStream<'a> {
        Box::pin(async_stream::stream! {
            match self.complete(system_prompt, messages).await {
                Ok(msg)  => yield Ok(msg.content.clone()),
                Err(e)   => yield Err(e),
            }
        })
    }
}
