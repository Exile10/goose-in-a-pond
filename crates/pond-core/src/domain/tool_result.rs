//! Domain type for tool execution results.
//!
//! Used by the multi-tool parallel dispatch pipeline to represent
//! the output of each individual tool invocation.

/// Result of a single tool execution.
///
/// Returned by `ToolAgent::process_multi()` to represent each tool's
/// output in a multi-tool dispatch. The `tool_name` identifies which
/// tool produced the result (e.g. "weather", "schedules") and `content`
/// holds the tool's output text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// Name of the tool that produced this result (e.g. "weather", "schedules").
    pub tool_name: String,
    /// The tool's output content.
    pub content: String,
}

impl ToolResult {
    /// Create a new tool result.
    pub fn new(tool_name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            content: content.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_new() {
        let r = ToolResult::new("weather", "Sunny, 25C");
        assert_eq!(r.tool_name, "weather");
        assert_eq!(r.content, "Sunny, 25C");
    }

    #[test]
    fn tool_result_clone_and_eq() {
        let a = ToolResult::new("weather", "data");
        let b = a.clone();
        assert_eq!(a, b);
    }
}
