//! MCP tool schema conversion to `ToolDefinition`.
//!
//! Bridges between MCP server tool listings and the `InferenceProvider`
//! tool format. Called at startup after listing tools from the MCP server.

use pond_core::models::ports::inference::ToolDefinition;

/// Convert an MCP tool (name + description + JSON Schema) into a `ToolDefinition`.
///
/// The `schema` parameter is the JSON Schema object from the MCP tool listing,
/// passed through directly as `parameters_schema`.
pub fn mcp_tool_to_definition(
    name: &str,
    description: &str,
    schema: serde_json::Value,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        parameters_schema: schema,
    }
}

/// Extract human-readable text from an MCP `CallToolResult` content array.
/// Each content item carries a `type`; only `"text"` items are concatenated
/// and every other type is skipped.
pub fn extract_tool_result_text(content: &[serde_json::Value]) -> String {
    let mut parts = Vec::new();
    for item in content {
        if item.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                parts.push(text);
            }
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tool_to_definition_basic() {
        let def = mcp_tool_to_definition(
            "get_weather",
            "Fetch current weather",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                },
                "required": ["location"]
            }),
        );
        assert_eq!(def.name, "get_weather");
        assert_eq!(def.description, "Fetch current weather");
        assert_eq!(
            def.parameters_schema["properties"]["location"]["type"],
            "string"
        );
    }

    #[test]
    fn extract_text_from_mixed_content() {
        let content = vec![
            serde_json::json!({"type": "text", "text": "Hello"}),
            serde_json::json!({"type": "image", "data": "base64..."}),
            serde_json::json!({"type": "text", "text": "World"}),
        ];
        assert_eq!(extract_tool_result_text(&content), "Hello\nWorld");
    }

    #[test]
    fn extract_text_from_empty_content() {
        let content: Vec<serde_json::Value> = vec![];
        assert_eq!(extract_tool_result_text(&content), "");
    }

    #[test]
    fn extract_text_skips_non_text_types() {
        let content = vec![
            serde_json::json!({"type": "image", "data": "..."}),
            serde_json::json!({"type": "resource", "uri": "file://..."}),
        ];
        assert_eq!(extract_tool_result_text(&content), "");
    }

    #[test]
    fn extract_text_handles_missing_text_field() {
        let content = vec![serde_json::json!({"type": "text"})];
        assert_eq!(extract_tool_result_text(&content), "");
    }
}
