use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

/// A tool-call result emitted to the canvas for rendering a ContextCard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool: String,
    pub data: serde_json::Value,
    pub timestamp_ms: u64,
}

/// Payload emitted when a new response token arrives (streaming).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseToken {
    pub token: String,
    pub done: bool,
}

/// Parse a single SSE line from the chat/stream endpoint and emit the
/// appropriate Tauri event to the canvas window.
///
/// Expected SSE data formats (JSON):
///   {"type": "text", "content": "Hello"}           → response-token event
///   {"type": "tool_call", "tool": "...", "result": {...}} → tool-result event
pub fn dispatch_sse_event(app: &AppHandle, line: &str) {
    let line = line.strip_prefix("data: ").unwrap_or(line).trim();
    if line.is_empty() || line == "[DONE]" {
        if line == "[DONE]" {
            let _ = app.emit(
                "response-token",
                ResponseToken {
                    token: String::new(),
                    done: true,
                },
            );
        }
        return;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };

    match value.get("type").and_then(|t| t.as_str()) {
        Some("text") => {
            if let Some(content) = value.get("content").and_then(|c| c.as_str()) {
                let _ = app.emit(
                    "response-token",
                    ResponseToken {
                        token: content.to_string(),
                        done: false,
                    },
                );
            }
        }
        Some("tool_call") => {
            let tool = value
                .get("tool")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();
            let data = value
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            let _ = app.emit(
                "tool-result",
                ToolResult {
                    tool,
                    data,
                    timestamp_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                },
            );
        }
        _ => {}
    }
}
