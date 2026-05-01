//! Tool context formatting — shared between the parallel-prep path in
//! routes.rs and the ToolAgent port implementation.

use pond_core::domain::tool_result::ToolResult;

/// Format tool results into attributed context for the main LLM.
///
/// Each tool type gets a compact, purpose-built template. The `[Tool: X]`
/// attribution lets the LLM see which tool produced the data and what was
/// queried, enabling it to reason about the source and request follow-ups.
pub fn format_tool_context(tool: &str, query: &str, message: &str, info: &str) -> String {
    let label = if query.is_empty() {
        format!("[Tool: {}]", tool)
    } else {
        format!("[Tool: {} | Query: {}]", tool, query)
    };

    match tool {
        // Action confirmations: relay directly.
        "create_schedule" | "save_memory" | "devices" | "schedules" => {
            format!(
                "{}\n\n{}\n{}\n\n\
                Tell the user what happened. Be concise and natural.",
                message, label, info
            )
        }
        // Weather: compact context.
        "weather" => {
            format!(
                "{}\n\n{}\n{}\n\n\
                Report the weather naturally.",
                message, label, info
            )
        }
        // Memory recall: present recalled memories.
        "recall_memory" => {
            format!(
                "{}\n\n{}\n{}\n\n\
                Answer using these memories. Be natural and personal.",
                message, label, info
            )
        }
        // Wikipedia / knowledge lookups: truncate to ~2000 chars to
        // stay within small-model context budgets.
        _ => {
            let truncated = if info.len() > 2000 {
                format!("{}...", &info[..2000])
            } else {
                info.to_string()
            };
            format!(
                "{}\n\n{}\n{}\n\n\
                Answer using this retrieved data. Include specific facts.",
                message, label, truncated
            )
        }
    }
}

/// Format a tool failure notice for the main LLM.
///
/// When a tool was classified as needed but returned no result, this gives
/// the LLM context about what was attempted so it can answer from its own
/// knowledge or suggest the user rephrase.
pub fn format_tool_failure(tool: &str, query: &str, message: &str) -> String {
    let label = if query.is_empty() {
        format!("[Tool: {} | Status: no result]", tool)
    } else {
        format!("[Tool: {} | Query: {} | Status: no result]", tool, query)
    };

    format!(
        "{}\n\n{}\nThe {} tool did not return results for this query.\n\
        Answer from your own knowledge. If uncertain, say so.",
        message, label, tool
    )
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
        return format_tool_context(&r.tool_name, "", message, &r.content);
    }

    // Multiple results — labeled sections with tool attribution.
    let mut sections = String::new();
    for result in results {
        if !sections.is_empty() {
            sections.push('\n');
        }
        sections.push_str(&format!("[Tool: {}]\n{}\n", result.tool_name, result.content));
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
    fn format_tool_context_wikipedia_with_query() {
        let output = format_tool_context("wikipedia", "John Cena", "who is John Cena?", "He is a wrestler.");
        assert!(output.contains("[Tool: wikipedia | Query: John Cena]"));
        assert!(output.contains("He is a wrestler."));
        assert!(output.contains("who is John Cena?"));
    }

    #[test]
    fn format_tool_context_weather_no_query() {
        let output = format_tool_context("weather", "", "what's the weather?", "Sunny, 25C");
        assert!(output.contains("[Tool: weather]"));
        assert!(!output.contains("Query:"));
        assert!(output.contains("Sunny, 25C"));
    }

    #[test]
    fn format_tool_context_truncates_long_reference() {
        let long_info = "x".repeat(3000);
        let output = format_tool_context("wikipedia", "topic", "tell me about topic", &long_info);
        assert!(output.len() < 2200); // truncated to ~2000 + overhead
        assert!(output.contains("..."));
    }

    #[test]
    fn format_tool_failure_with_query() {
        let output = format_tool_failure("wikipedia", "nonexistent", "who is nonexistent?");
        assert!(output.contains("[Tool: wikipedia | Query: nonexistent | Status: no result]"));
        assert!(output.contains("did not return results"));
        assert!(output.contains("Answer from your own knowledge"));
    }

    #[test]
    fn format_tool_failure_without_query() {
        let output = format_tool_failure("weather", "", "what's the weather?");
        assert!(output.contains("[Tool: weather | Status: no result]"));
    }

    #[test]
    fn format_multi_empty() {
        let result = format_multi_tool_context("hello", &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn format_multi_single_delegates_to_single_formatter() {
        let results = vec![ToolResult::new("weather", "Sunny, 25C")];
        let output = format_multi_tool_context("what's the weather", &results);
        assert!(output.contains("[Tool: weather]"));
        assert!(output.contains("Sunny, 25C"));
    }

    #[test]
    fn format_multi_two_tools_produces_labeled_sections() {
        let results = vec![
            ToolResult::new("weather", "Sunny, 25C"),
            ToolResult::new("schedules", "- Briefing at 8am"),
        ];
        let output = format_multi_tool_context("weather and schedules", &results);
        assert!(output.contains("[Tool: weather]"));
        assert!(output.contains("Sunny, 25C"));
        assert!(output.contains("[Tool: schedules]"));
        assert!(output.contains("- Briefing at 8am"));
        assert!(output.contains("Address each part"));
    }
}

