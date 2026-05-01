//! Tool context formatting — shared between the parallel-prep path in
//! routes.rs and the ToolAgent port implementation.

use pond_core::domain::tool_result::ToolResult;

/// Format tool results into context for the main LLM.
///
/// Each tool type gets a compact, purpose-built template that avoids
/// bloated instructions which overwhelm small models' context windows.
pub fn format_tool_context(tool: &str, message: &str, info: &str) -> String {
    match tool {
        // Action confirmations: relay directly, no "authoritative source" framing.
        "create_schedule" | "save_memory" | "devices" | "schedules" => {
            format!(
                "{}\n\n[Action result]\n{}\n\n\
                Tell the user what happened. Be concise and natural.",
                message, info
            )
        }
        // Weather: compact context.
        "weather" => {
            format!(
                "{}\n\n[Current weather data]\n{}\n\n\
                Report the weather naturally. No need to mention the data source.",
                message, info
            )
        }
        // Memory recall: present recalled memories.
        "recall_memory" => {
            format!(
                "{}\n\n[Recalled memories]\n{}\n\n\
                Answer using these memories. Be natural and personal.",
                message, info
            )
        }
        // Wikipedia / knowledge lookups: truncate to ~2000 chars to
        // stay within small-model context budgets.
        _ => {
            let truncated = if info.len() > 2000 {
                format!("{}…", &info[..2000])
            } else {
                info.to_string()
            };
            format!(
                "{}\n\n[Reference]\n{}\n\n\
                Answer using the reference above. Include specific facts. \
                Do not mention that anything was looked up.",
                message, truncated
            )
        }
    }
}

/// Format multiple tool results into a combined context string.
///
/// Each tool result is presented as a labeled section so the LLM can
/// distinguish between different data sources. Used by the multi-tool
/// parallel dispatch pipeline when `multi_tool_enabled` is true.
///
/// Example output:
/// ```text
/// [weather]
/// Sunny, 25C in Nairobi
///
/// [schedules]
/// - Daily briefing at 8am
/// ```
pub fn format_multi_tool_context(message: &str, results: &[ToolResult]) -> String {
    if results.is_empty() {
        return String::new();
    }

    // Single result — delegate to the existing single-tool formatter
    // for consistent formatting with the non-multi path.
    if results.len() == 1 {
        let r = &results[0];
        return format_tool_context(&r.tool_name, message, &r.content);
    }

    // Multiple results — labeled sections.
    let mut sections = String::new();
    for result in results {
        if !sections.is_empty() {
            sections.push('\n');
        }
        sections.push_str(&format!("[{}]\n{}\n", result.tool_name, result.content));
    }

    format!(
        "{}\n\n{}\n\
        Answer the user's question using all the information above. \
        Address each part of their request. Be concise and natural.",
        message, sections
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_multi_empty() {
        let result = format_multi_tool_context("hello", &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn format_multi_single_delegates_to_single_formatter() {
        let results = vec![ToolResult::new("weather", "Sunny, 25C")];
        let output = format_multi_tool_context("what's the weather", &results);
        // Should match single-tool format (weather template)
        assert!(output.contains("[Current weather data]"));
        assert!(output.contains("Sunny, 25C"));
    }

    #[test]
    fn format_multi_two_tools_produces_labeled_sections() {
        let results = vec![
            ToolResult::new("weather", "Sunny, 25C"),
            ToolResult::new("schedules", "- Briefing at 8am"),
        ];
        let output = format_multi_tool_context("weather and schedules", &results);
        assert!(output.contains("[weather]"));
        assert!(output.contains("Sunny, 25C"));
        assert!(output.contains("[schedules]"));
        assert!(output.contains("- Briefing at 8am"));
        assert!(output.contains("Address each part"));
    }
}

