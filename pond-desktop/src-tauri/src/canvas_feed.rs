use serde::{Deserialize, Serialize};
use std::sync::Mutex;
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

/// Payload emitted when the backend finalizes the session for this stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreated {
    pub session_id: String,
    pub model_role: String,
}

/// Persistent stateful filter for cross-chunk thinking tag stripping.
/// Reset at the start of each new chat stream. Handles all tag formats:
/// `<think>`, `<thought>`, `<|channel>thought`, `<|tool_call>`, and
/// standalone sentinels (`<eos>`, `<end_of_turn>`, etc.).
static THOUGHT_FILTER: Mutex<Option<crate::thought_filter::ThoughtFilter>> = Mutex::new(None);

/// Reset the thinking filter — call at the start of each new stream.
pub fn reset_think_filter() {
    *THOUGHT_FILTER.lock().unwrap() = Some(crate::thought_filter::ThoughtFilter::new());
}

/// Filter thinking/reasoning tags from a streaming text chunk.
fn filter_thinking(text: &str) -> String {
    let mut guard = THOUGHT_FILTER.lock().unwrap();
    let filter = guard.get_or_insert_with(crate::thought_filter::ThoughtFilter::new);
    filter.push(text)
}

/// Parse a single SSE line from the chat/stream endpoint and emit the
/// appropriate Tauri event to the canvas window.
///
/// Supported SSE data formats (JSON):
///   {"type": "text", "content": "Hello"}                        → response-token event
///   {"token": "Hello"}                                          → response-token event (legacy/compat)
///   {"type": "tool_call", "tool": "...", "result": {...}}       → tool-result event
///   {"done": true, "session_id": "...", "model_role": "..."}    → session-created event
///   {"error": "..."}                                            → pipeline-error event
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

    // Handle done event — emitted by backend at end of stream with session_id.
    if value.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
        reset_think_filter();
        let session_id = value
            .get("session_id")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let model_role = value
            .get("model_role")
            .and_then(|r| r.as_str())
            .unwrap_or("chat")
            .to_string();
        // Signal stream end
        let _ = app.emit(
            "response-token",
            ResponseToken {
                token: String::new(),
                done: true,
            },
        );
        if !session_id.is_empty() {
            let _ = app.emit(
                "session-created",
                SessionCreated {
                    session_id,
                    model_role,
                },
            );
        }
        return;
    }

    // Handle error event
    if let Some(err) = value.get("error").and_then(|e| e.as_str()) {
        let _ = app.emit("pipeline-error", err.to_string());
        return;
    }

    match value.get("type").and_then(|t| t.as_str()) {
        Some("text") => {
            if let Some(content) = value.get("content").and_then(|c| c.as_str()) {
                let visible = filter_thinking(content);
                if !visible.is_empty() {
                    let _ = app.emit(
                        "response-token",
                        ResponseToken {
                            token: visible,
                            done: false,
                        },
                    );
                }
            }
        }
        Some("tool_call") => {
            let tool = value
                .get("tool")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();
            let data = value
                .get("input")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));

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
        Some("tool_result") => {
            let tool = value
                .get("tool")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();
            // tool_result has the actual output in "content"
            let content = value.get("content").and_then(|c| c.as_str()).unwrap_or("");
            let data = if content.is_empty() {
                serde_json::json!({})
            } else {
                // Try parsing as JSON; fall back to wrapping as {"result": "..."}
                serde_json::from_str::<serde_json::Value>(content)
                    .unwrap_or_else(|_| serde_json::json!({"result": content}))
            };

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
        _ => {
            // Legacy/compat: handle {"token": "..."} format emitted by older backends.
            if let Some(token) = value.get("token").and_then(|t| t.as_str()) {
                let visible = filter_thinking(token);
                if !visible.is_empty() {
                    let _ = app.emit(
                        "response-token",
                        ResponseToken {
                            token: visible,
                            done: false,
                        },
                    );
                }
            }
        }
    }
}
