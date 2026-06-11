//! Port trait for the dynamic tool registry.
//!
//! Provides a unified view of all available tools (built-in GIAP tools
//! and external MCP extension tools) for system prompt construction
//! and tool resolution.

use async_trait::async_trait;

use crate::mcp::domain::external_tool::ExternalToolDescription;

/// Registry of all tool descriptions available to the agent pipeline.
///
/// Built-in tools are seeded at construction. External MCP extension
/// tools are registered/deregistered as extensions are added/removed.
#[async_trait]
pub trait ToolRegistryPort: Send + Sync {
    /// Return all registered tool descriptions (built-in + external).
    async fn all_tools(&self) -> Vec<ExternalToolDescription>;

    /// Return formatted description lines suitable for system prompt injection.
    ///
    /// Built-in tools: `"tool_name -- description"`
    /// External tools: `"extension/tool_name -- description"`
    ///
    /// When `compact` is true, descriptions are truncated to 80 characters.
    async fn prompt_description_lines(&self, compact: bool) -> Vec<String>;

    /// Register tools from an external MCP extension.
    ///
    /// `tools` is a list of `(tool_name, description)` pairs.
    /// Replaces any previously registered tools for the same extension.
    async fn register_extension_tools(&self, ext_name: &str, tools: Vec<(String, String)>);

    /// Remove all tools from a given extension.
    async fn deregister_extension(&self, ext_name: &str);

    /// Look up which extension provides a given tool name.
    ///
    /// Returns the extension name, or `None` if the tool is not registered.
    async fn resolve_extension(&self, tool_name: &str) -> Option<String>;
}
