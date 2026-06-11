//! External tool descriptions for the dynamic tool registry.
//!
//! Pure domain type — no external dependencies.

/// Description of a tool available in the agent pipeline.
///
/// Built-in GIAP tools (wikipedia, weather, memory, etc.) and tools
/// from external MCP extensions share the same description type so
/// they can be rendered together in the system prompt.
#[derive(Debug, Clone)]
pub struct ExternalToolDescription {
    /// Tool name as it appears in the MCP schema (e.g. "wikipedia", "get_weather").
    pub tool_name: String,
    /// Name of the extension that provides this tool (e.g. "giap", "filesystem-server").
    pub extension_name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// True for GIAP's built-in tools, false for user-added MCP extensions.
    pub is_builtin: bool,
}
