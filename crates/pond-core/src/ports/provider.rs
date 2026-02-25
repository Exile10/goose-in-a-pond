use anyhow::Result;
use async_trait::async_trait;
use thiserror::Error;

pub use crate::domain::message::ChatMessage;

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Provider error: {0}")]
    General(String),

    #[error("Model not available: {0}")]
    ModelNotAvailable(String),
}

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
}
