use crate::models::domain::model_capabilities::ModelCapabilities;
pub use crate::shared::domain::agent::{AgentRequest, AgentResponse, AgentStreamEvent};
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("General error: {0}")]
    General(String),
}

/// Driven Port: Agent
///
/// This trait defines the interface for interacting with an AI agent.
#[async_trait]
pub trait Agent: Send + Sync {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse>;

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>>;

    /// Runtime capabilities of the model backing this agent.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    /// Call a tool directly by its fully-qualified name (e.g. "giap-weather__get_current_weather").
    ///
    /// Used as a fallback when the model emits tool calls as text markup
    /// (`<|tool_call>...<tool_call|>`) instead of through the structured protocol.
    /// Returns the tool result text, or an error if the tool is not found.
    async fn call_tool(
        &self,
        _session_id: &str,
        _tool_name: &str,
        _args_json: &str,
    ) -> Result<String> {
        Err(anyhow::anyhow!("call_tool not supported by this agent"))
    }
}
