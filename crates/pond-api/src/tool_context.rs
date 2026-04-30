//! Tool context formatting — shared between the parallel-prep path in
//! routes.rs and the ToolAgent port implementation.

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
