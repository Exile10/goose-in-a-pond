//! ToolDispatcher port — dispatches the tool calls `PondAgent` detects in the LLM's output,
//! routing by tool name prefix (e.g. "giap-weather__get_current_weather") to the owning MCP server.

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

/// Driven port: dispatches tool calls to registered MCP tool servers. Implementations route by
/// tool name and return the tool's response as text content for the conversation history.
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

    /// Tool definitions pre-formatted as an OpenAI-compatible JSON string, byte-identical to what
    /// Goose's `format_tools()` produces and ready to pass to `apply_chat_template_oaicompat()` as
    /// `tools_json`, bypassing any conversion that could alter the schema structure.
    async fn tools_json(&self) -> Option<String> {
        let defs = self.available_tool_definitions().await;
        if defs.is_empty() {
            return None;
        }
        let specs: Vec<serde_json::Value> = defs
            .into_iter()
            .map(|(name, desc, schema)| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": desc,
                        "parameters": schema,
                    }
                })
            })
            .collect();
        serde_json::to_string(&specs).ok()
    }

    /// Return compact tool definitions (name + description only, no schemas).
    /// Used as fallback when full schemas exceed the token budget.
    async fn compact_tools_json(&self) -> Option<String> {
        let defs = self.available_tool_definitions().await;
        if defs.is_empty() {
            return None;
        }
        let specs: Vec<serde_json::Value> = defs
            .into_iter()
            .map(|(name, desc, _)| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": desc,
                    }
                })
            })
            .collect();
        serde_json::to_string(&specs).ok()
    }
}
