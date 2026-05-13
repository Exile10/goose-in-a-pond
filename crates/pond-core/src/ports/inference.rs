//! InferenceProvider port — tool-aware chat completion interface.
//!
//! Separate from [`LlmProvider`](super::provider::LlmProvider) which handles
//! simple completions (memory extraction, answer review). This port adds
//! native tool-calling support for the agent loop.

use crate::domain::message::ChatMessage;
use crate::domain::model_capabilities::ModelCapabilities;
use crate::ports::provider::UsageStats;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// Event emitted during a streaming chat completion.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// A text token from the model.
    Text(String),
    /// The model requests a tool call.
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// End-of-generation token usage stats.
    Usage(UsageStats),
}

/// A tool the model may call during inference.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Tool name (e.g. `"get_current_weather"`).
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    pub parameters_schema: serde_json::Value,
}

/// Options controlling inference behaviour.
#[derive(Debug, Clone, Default)]
pub struct InferenceOptions {
    /// Maximum tokens to generate.
    pub max_tokens: Option<u32>,
    /// Sampling temperature (0.0 = deterministic).
    pub temperature: Option<f32>,
}

/// A pinned, boxed stream of [`ChatEvent`] items.
pub type ChatEventStream = Pin<Box<dyn Stream<Item = Result<ChatEvent>> + Send>>;

/// Driven Port: InferenceProvider
///
/// Abstraction for LLM inference with native tool-calling support.
/// Both HTTP providers (Ollama, llamafile) and in-process providers
/// (llama.cpp via GGUF) implement this trait.
///
/// The agent loop calls [`stream_chat`] in a loop: when the model emits
/// [`ChatEvent::ToolCall`] events, the agent executes the tools, appends
/// results to the message history, and calls `stream_chat` again. The
/// loop terminates when no tool calls are emitted (model produced a
/// final text answer) or a max-iteration guard fires.
#[async_trait]
pub trait InferenceProvider: Send + Sync {
    /// Stream a chat completion with optional tool definitions.
    ///
    /// If `tools` is non-empty and the model supports tool calling,
    /// the stream may emit [`ChatEvent::ToolCall`] events. Otherwise
    /// only [`ChatEvent::Text`] and [`ChatEvent::Usage`] are emitted.
    fn stream_chat(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &InferenceOptions,
    ) -> ChatEventStream;

    /// The name of the underlying model (e.g. `"gemma4:e4b"`).
    fn model_name(&self) -> String;

    /// Runtime capabilities of the underlying model.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}
