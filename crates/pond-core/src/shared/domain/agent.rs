use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRequest {
    pub message: String,
    pub session_id: String,
    pub model_role: String, // "chat" | "think" | "task"
    /// Optional image attachments for multimodal models.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::models::domain::message::ImageAttachment>,
    /// When true, the request originates from voice mode. The agent should
    /// disable thinking, keep responses concise, and avoid formatting.
    #[serde(default)]
    pub voice_mode: bool,
    /// When true, the request originates from Canvas mode. The agent should
    /// always prefer tool calls over textual descriptions so that results
    /// render as visual cards on the user's screen.
    #[serde(default)]
    pub canvas_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub text: String,
    pub metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentStreamEvent {
    Status {
        content: String,
    },
    /// Internal reasoning / chain-of-thought from models that support thinking
    /// (Gemma 4, Qwen3, DeepSeek-R1). Only emitted when `show_thinking` is enabled.
    Thinking {
        content: String,
    },
    ToolCall {
        id: String,
        tool: String,
        input: Option<serde_json::Value>,
    },
    ToolResult {
        id: String,
        tool: String,
        content: String,
    },
    Text {
        content: String,
    },
    /// Adversarial review status — emitted during post-inference answer review.
    ReviewStatus {
        content: String,
    },
    /// Revised answer from the adversarial reviewer.
    /// The frontend should replace the previously streamed text with this content.
    ReviewRevision {
        content: String,
        score: u8,
        rounds: u32,
    },
    Done {
        session_id: String,
        model_role: String,
        /// Token usage for this response (estimated if real counts unavailable).
        usage: Option<crate::models::ports::provider::UsageStats>,
        /// Per-turn inference performance stats (TTFT, prefill/decode tok/s,
        /// context utilization). None when the engine reports nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stats: Option<super::turn_stats::TurnStats>,
    },
    Error {
        content: String,
    },
}

/// The four states of the workflow loop.
///
/// Serializes to the contract's snake_case state strings
/// (`wait` / `listen` / `thinking` / `speak`) so
/// [`WorkflowEvent::StateChanged`] flattens to `{"event":"state","state":"wait"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    /// Idle – ready for the next interaction.
    Wait,
    /// Receiving user input.
    Listen,
    /// Processing the input through the agent.
    Thinking,
    /// Delivering the agent's response.
    Speak,
}

impl fmt::Display for WorkflowState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wait => write!(f, "Wait"),
            Self::Listen => write!(f, "Listen"),
            Self::Thinking => write!(f, "Thinking"),
            Self::Speak => write!(f, "Speak"),
        }
    }
}

/// Events emitted during the workflow loop (for UI / logging / NDJSON hooks).
///
/// # NDJSON contract (terminal-voice-in-desktop, Architecture A)
///
/// The variants that carry an external contract payload serialize — via
/// `serde_json::to_string` — to EXACTLY the shapes the Tauri shell parses off
/// the child's stdout (one JSON object per line, `snake_case`), tagged with an
/// `"event"` field:
///
/// ```text
/// {"event":"ready","session_id":"<uuid>"}
/// {"event":"state","state":"wait"}            // wait | listen | thinking | speak
/// {"event":"transcript","text":"..."}
/// {"event":"token","content":"..."}
/// {"event":"tool_call","tool":"giap__x","id":"..."}
/// {"event":"tool_result","tool":"giap__x","id":"...","content":"..."}
/// {"event":"turn_complete","session_id":"<uuid>"}
/// {"event":"error","message":"..."}
/// {"event":"exit","reason":"stdin_eof"}       // stdin_eof | dismissed | error
/// ```
///
/// `UserInput` and `AgentOutput` are legacy internal-only variants retained for
/// the tracing hook and existing call sites; they carry no external contract
/// shape and the NDJSON sink deliberately skips them (the streamed `Token`
/// deltas and `Transcript` already cover that information for the UI).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkflowEvent {
    /// Emitted once after models are loaded, before entering the wait loop.
    Ready { session_id: String },
    /// A workflow state transition. Serializes as
    /// `{"event":"state","state":"wait"}` — the variant is tagged `state` and
    /// the wrapped [`WorkflowState`] serializes to the snake_case state string.
    #[serde(rename = "state")]
    StateChanged { state: WorkflowState },
    /// A confirmed user utterance (post-ASR).
    Transcript { text: String },
    /// An assistant token delta.
    Token { content: String },
    /// An MCP tool invocation is starting.
    ToolCall { tool: String, id: String },
    /// An MCP tool returned. `content` is truncated to 2000 chars by the emitter.
    ToolResult {
        tool: String,
        id: String,
        content: String,
    },
    /// The turn finished and was persisted exactly once.
    TurnComplete { session_id: String },
    /// A recoverable error occurred during the turn.
    Error { message: String },
    /// The loop is exiting cleanly. `reason` is `stdin_eof` | `dismissed` | `error`.
    Exit { reason: String },
    /// Legacy internal-only: user input text. Carries no external contract shape;
    /// [`WorkflowEvent::to_ndjson`] returns `None` for it so the sink skips it.
    UserInput(String),
    /// Legacy internal-only: full agent output text. Carries no external contract
    /// shape; [`WorkflowEvent::to_ndjson`] returns `None` for it so the sink skips it.
    AgentOutput(String),
}

impl WorkflowEvent {
    /// Serialize this event to a single NDJSON line for the child-process
    /// stdout contract, or `None` for the legacy internal-only variants
    /// (`UserInput` / `AgentOutput`) that carry no external contract shape.
    ///
    /// The returned string is a single line with NO trailing newline — the
    /// writer appends `\n` and flushes. `serde_json` never emits interior
    /// newlines for these shapes, so one event maps to exactly one line.
    pub fn to_ndjson(&self) -> Option<String> {
        match self {
            Self::UserInput(_) | Self::AgentOutput(_) => None,
            other => serde_json::to_string(other).ok(),
        }
    }
}

// ── Golden NDJSON serializer tests ──────────────────────────────────────────
//
// These assert the EXACT JSON strings the terminal-voice-in-desktop contract
// (Architecture A) specifies. The Tauri shell parses these off child stdout;
// any drift here breaks the parser, so the strings are pinned byte-for-byte.
#[cfg(test)]
mod ndjson_golden_tests {
    use super::*;

    #[test]
    fn ready_line_matches_contract() {
        let ev = WorkflowEvent::Ready {
            session_id: "abc-123".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"ready","session_id":"abc-123"}"#
        );
    }

    #[test]
    fn state_lines_match_contract_snake_case() {
        let cases = [
            (WorkflowState::Wait, r#"{"event":"state","state":"wait"}"#),
            (
                WorkflowState::Listen,
                r#"{"event":"state","state":"listen"}"#,
            ),
            (
                WorkflowState::Thinking,
                r#"{"event":"state","state":"thinking"}"#,
            ),
            (WorkflowState::Speak, r#"{"event":"state","state":"speak"}"#),
        ];
        for (state, expected) in cases {
            let ev = WorkflowEvent::StateChanged { state };
            assert_eq!(ev.to_ndjson().unwrap(), expected, "state {:?}", state);
        }
    }

    #[test]
    fn transcript_line_matches_contract() {
        let ev = WorkflowEvent::Transcript {
            text: "what's the weather".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"transcript","text":"what's the weather"}"#
        );
    }

    #[test]
    fn token_line_matches_contract() {
        let ev = WorkflowEvent::Token {
            content: "Hello".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"token","content":"Hello"}"#
        );
    }

    #[test]
    fn tool_call_line_matches_contract() {
        let ev = WorkflowEvent::ToolCall {
            tool: "giap__get_weather".to_string(),
            id: "call-1".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"tool_call","tool":"giap__get_weather","id":"call-1"}"#
        );
    }

    #[test]
    fn tool_result_line_matches_contract() {
        let ev = WorkflowEvent::ToolResult {
            tool: "giap__get_weather".to_string(),
            id: "call-1".to_string(),
            content: "sunny".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"tool_result","tool":"giap__get_weather","id":"call-1","content":"sunny"}"#
        );
    }

    #[test]
    fn turn_complete_line_matches_contract() {
        let ev = WorkflowEvent::TurnComplete {
            session_id: "abc-123".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"turn_complete","session_id":"abc-123"}"#
        );
    }

    #[test]
    fn error_line_matches_contract() {
        let ev = WorkflowEvent::Error {
            message: "boom".to_string(),
        };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"error","message":"boom"}"#
        );
    }

    #[test]
    fn exit_line_matches_contract() {
        for reason in ["stdin_eof", "dismissed", "error"] {
            let ev = WorkflowEvent::Exit {
                reason: reason.to_string(),
            };
            assert_eq!(
                ev.to_ndjson().unwrap(),
                format!(r#"{{"event":"exit","reason":"{}"}}"#, reason)
            );
        }
    }

    #[test]
    fn legacy_variants_are_not_serialized_to_ndjson() {
        assert!(WorkflowEvent::UserInput("hi".to_string())
            .to_ndjson()
            .is_none());
        assert!(WorkflowEvent::AgentOutput("out".to_string())
            .to_ndjson()
            .is_none());
    }

    #[test]
    fn ndjson_lines_contain_no_interior_newlines() {
        // One event must serialize to exactly one line.
        let ev = WorkflowEvent::Token {
            content: "a\nb".to_string(),
        };
        let line = ev.to_ndjson().unwrap();
        // The embedded newline in content must be escaped, not literal.
        assert!(!line.contains('\n'), "line had a raw newline: {line:?}");
        assert!(line.contains("\\n"));
    }
}
