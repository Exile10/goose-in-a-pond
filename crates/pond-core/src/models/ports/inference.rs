//! InferenceProvider port — tool-aware chat completion. Separate from
//! [`LlmProvider`](super::provider::LlmProvider), which handles the simple completions (memory
//! extraction, answer review); this port adds native tool calling for the agent loop.

use crate::models::domain::message::ChatMessage;
use crate::models::domain::model_capabilities::ModelCapabilities;
use crate::models::ports::provider::UsageStats;
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
    /// Enable model thinking/reasoning (produces `<|channel>thought...<channel|>` tags).
    pub enable_thinking: bool,
    /// Pre-formatted OpenAI-compatible tools JSON string from the dispatcher.
    /// When set, the provider uses this directly instead of re-serializing ToolDefinition objects.
    /// This matches Goose's `format_tools()` output exactly.
    pub tools_json_override: Option<String>,
    /// Pre-formatted compact tools JSON (name + description only, no schemas).
    pub compact_tools_json_override: Option<String>,
}

/// A pinned, boxed stream of [`ChatEvent`] items.
pub type ChatEventStream = Pin<Box<dyn Stream<Item = Result<ChatEvent>> + Send>>;

/// Driven Port: InferenceProvider — LLM inference with native tool calling, implemented by both
/// HTTP providers (Ollama, llamafile) and in-process ones (llama.cpp via GGUF). The agent loop
/// calls [`stream_chat`] repeatedly, executing each [`ChatEvent::ToolCall`] and appending its
/// result, until a turn emits no tool calls or the max-iteration guard fires.
#[async_trait]
pub trait InferenceProvider: Send + Sync {
    /// Stream a chat completion with optional tool definitions. If `tools` is non-empty and the
    /// model supports tool calling the stream may emit [`ChatEvent::ToolCall`]; otherwise only
    /// [`ChatEvent::Text`] and [`ChatEvent::Usage`] are emitted.
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
