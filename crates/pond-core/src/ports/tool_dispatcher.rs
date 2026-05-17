//! ToolDispatcher port — dispatches tool calls to MCP servers.
//!
//! Used by `PondAgent` to execute tool calls detected in the LLM's output.
//! The implementation routes calls to the appropriate MCP server based on
//! tool name prefix (e.g. "giap-weather__get_current_weather").

use anyhow::Result;
use async_trait::async_trait;

/// Result of dispatching a tool call to an MCP server.
#[derive(Debug, Clone)]
pub struct ToolCallResult {
    /// The text content returned by the tool (may include UI hints).
    pub content: String,
    /// Whether the tool execution was successful.
    pub success: bool,
}

/// Driven port: dispatches tool calls to registered MCP tool servers.
///
/// Implementations route by tool name to the correct MCP server and
/// return the tool's response as text content for injection into the
/// conversation history.
#[async_trait]
pub trait ToolDispatcher: Send + Sync {
    /// Dispatch a tool call and return its result.
    ///
    /// `tool_name` is the full qualified name (e.g. "giap-weather__get_current_weather").
    /// `arguments` is the JSON arguments object from the LLM's tool call.
    async fn dispatch(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolCallResult>;

    /// List all available tool names (for validation / filtering).
    async fn available_tools(&self) -> Vec<String>;

    /// List all available tools with descriptions and parameter schemas.
    /// Returns `(name, description, parameters_json_schema)` from the MCP server schemas.
    async fn available_tool_definitions(&self) -> Vec<(String, String, serde_json::Value)> {
        // Default: return names with empty descriptions and empty schemas.
        self.available_tools()
            .await
            .into_iter()
            .map(|name| (name, String::new(), serde_json::json!({})))
            .collect()
    }
}
