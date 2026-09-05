//! ToolCaller port — driven port for generating tool-call arguments via a specialist model.

use anyhow::Result;
use async_trait::async_trait;

/// Driven port: tool-call argument generation. When the main LLM calls an MCP tool but produces
/// invalid arguments (common with small local GGUF models), this specialist regenerates them from
/// the tool schema and the user's request. It sees no conversation history, which keeps it fast.
#[async_trait]
pub trait ToolCaller: Send + Sync {
    /// Generate tool-call arguments for the given tool and user query.
    ///
    /// Returns the arguments as a JSON map (e.g. `{"topic": "Kenya"}`).
    /// The caller is responsible for passing these to the MCP tool.
    async fn generate_tool_call(
        &self,
        tool_name: &str,
        tool_schema_json: &str,
        user_query: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>>;
}
