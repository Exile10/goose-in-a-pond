use crate::user_data::domain::profile::ProfileScope;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Mutex;

/// One step of the boot-time prefix warm-up (see `Agent::prewarm`).
///
/// Three states, not a percentage: the engine exposes no progress inside a
/// model load or a prefill, and a bar that invents numbers is worse than one
/// that says what it knows. `Warming` covers everything between "asked" and
/// "the first token came back"; the UI renders it as an indeterminate bar
/// with elapsed time, and voice mode speaks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WarmupPhase {
    /// The warm-up generation is running: model load + prompt-prefix prefill.
    Warming,
    /// The prefix is resident in the engine's KV cache; turn 1 will reuse it.
    Ready,
    /// Nothing to warm on this backend (mock, HTTP providers) or explicitly
    /// disabled. The UI shows nothing; voice skips the "warming up" line.
    Skipped { reason: String },
    /// The warm-up generation failed. Chat still works — the first real turn
    /// simply pays the full prefill, exactly as before this feature existed.
    Failed { reason: String },
}

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
    /// Whose data this turn may reach.
    ///
    /// Resolved once, at the edge, by
    /// [`identity_resolution::resolve`](crate::user_data::services::identity_resolution::resolve),
    /// and carried down rather than recomputed -- two resolutions of one turn
    /// could disagree, and the one nearer the data would win.
    ///
    /// There is **no `Default` on `AgentRequest`** and this field is not
    /// optional in Rust, so every construction site has to say what it means.
    /// That is deliberate: a scope is an authorisation decision, and the
    /// compile error when a new caller appears is the review. The serde default
    /// exists only so a request serialized before this field existed still
    /// deserializes, and it reproduces the pre-PAI-1 behaviour exactly.
    #[serde(default = "ProfileScope::household")]
    pub profile_scope: ProfileScope,
    /// The speaking member's own preferences, resolved alongside the scope.
    ///
    /// `None` means "no personal context in this prompt" -- which is what a
    /// `Guest` turn gets, and what a pond with no primary member set has always
    /// got. It is deliberately not "fall back to whoever is primary": using one
    /// member's name and language while a different member is talking is the
    /// wrong-attribution failure in its most visible form.
    ///
    /// Rides the request rather than being fetched in the adapter, for the same
    /// reason as `profile_scope`: resolved once, at the edge, by the layer that
    /// can actually reach a `ProfileRepository`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_context: Option<crate::prompts::ProfileContext>,
    /// When set, restricts this turn to only the listed tool-group prefixes
    /// (e.g. `"giap-weather"`), on top of whatever tool selection would
    /// otherwise choose. Set by a recipe run whose YAML declares
    /// `extensions:`; `None` means "no recipe-imposed restriction" -- the
    /// ordinary case for chat turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_group_allowlist: Option<Vec<String>>,
    /// This turn is the boot-time prefix warm-up, not a person asking anything.
    ///
    /// There is nobody to be wrong at, so the completeness check
    /// (`Settings::goal_check_enabled`, armed in the Goose adapter) is not armed
    /// for it. That check is deliberately ON for real turns and deliberately
    /// costs roughly twice the inferences per turn -- measured, on a warm-up
    /// ping, as a second 4,188-token round-trip asking the model to "finish
    /// anything still outstanding" after it had already replied `ok`.
    ///
    /// Skipping it cannot move the warmed prefix, which is the only thing that
    /// would make this unsafe: arming the goal does not alter the FIRST
    /// request's payload -- it appends a nudge as a later user message, so it
    /// only ever adds a round-trip after the first one has finished. Verified
    /// from a captured payload pair (`GIAP_CAPTURE_PAYLOAD`).
    #[serde(default)]
    pub warmup: bool,
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
    /// The agent loop stopped because the request's turn budget was exhausted,
    /// not because the task was finished. Carries the budget that was hit so a
    /// client can say so and offer a one-click continuation turn — without this
    /// the user just gets the engine's "would you like me to continue?" as
    /// plain text with nothing wired to answer it.
    TurnLimitReached {
        max_turns: u32,
    },
    /// A delegation running underneath this turn changed state — PAI-6 P6.
    ///
    /// A delegating turn is otherwise a spinner: the parent is parked inside one
    /// `delegate` tool call for the whole of a child's run, which on-device is
    /// minutes, and nothing reaches the client until the tool result does. These
    /// frames are what let a client draw the tree instead.
    ///
    /// # What it may carry, and what it may not
    ///
    /// PAI-6 invariant 4 says a subagent's conversation never becomes the
    /// parent's history — only its final result, as a tool result. A progress
    /// frame is about a child, so it is a frame and nothing else: no consumer
    /// may fold `detail` into the turn's text or its persisted tool results.
    ///
    /// `detail` is deliberately narrow. It carries a tool NAME while a child is
    /// calling one, and a GIAP-authored reason when a run fails. It never
    /// carries the child's prose, its reasoning, or a tool call's ARGUMENTS —
    /// on this pond those can be a household memory query or device state
    /// (PAI-2 minimisation), and the producer that fills this field cannot
    /// express any of them. See `orchestrator.rs :: child_tool_names`.
    SubagentProgress {
        /// The `TaskRun` id, so a client can group frames per child rather than
        /// per role — one turn may delegate the same role twice.
        task_id: String,
        /// The role that was delegated to, for the label on the tree.
        role: String,
        status: SubagentStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
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

/// Where a delegation has got to, as a client sees it — PAI-6 P6.
///
/// A closed set rather than a free string, so a consumer that adds a branch is
/// told when a new state appears instead of silently rendering nothing. It is
/// deliberately NOT
/// [`TaskStatus`](crate::shared::domain::orchestration::TaskStatus): that enum
/// is the run's lifecycle and has no way to say "the child is calling a tool",
/// which is the state a delegating turn spends most of its wall clock in and the
/// only one that says anything is still happening.
///
/// [`From<TaskStatus>`](Self::from) is an exhaustive match, so a lifecycle state
/// added over there is a compile error here rather than a frame nobody drew.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    /// Authorised, waiting for the device. On-device this is where a second
    /// delegation from the same turn sits while its sibling runs.
    Queued,
    /// Holding the device and replying.
    Running,
    /// Calling a tool. `detail` is the tool's name and never its arguments.
    Tool,
    /// Finished with an answer, which arrives separately as the `delegate` tool
    /// result — never in a progress frame.
    Completed,
    /// Stopped, by the parent or by the parent's own turn ending.
    Cancelled,
    /// Ran out of turns. Whatever came back is a budget message, not an answer.
    TurnBudgetExhausted,
    /// Did not produce an answer. `detail` is GIAP's own reason.
    Failed,
}

impl SubagentStatus {
    /// Every variant, so a guard can iterate them rather than list them.
    pub const ALL: [SubagentStatus; 7] = [
        SubagentStatus::Queued,
        SubagentStatus::Running,
        SubagentStatus::Tool,
        SubagentStatus::Completed,
        SubagentStatus::Cancelled,
        SubagentStatus::TurnBudgetExhausted,
        SubagentStatus::Failed,
    ];

    /// The wire spelling, for a renderer that has no serializer to hand.
    ///
    /// `the_wire_spelling_is_the_serialized_spelling` pins this against serde
    /// for every variant. Two spellings of one state is how a client ends up
    /// with a branch that can never be true.
    pub fn as_str(self) -> &'static str {
        match self {
            SubagentStatus::Queued => "queued",
            SubagentStatus::Running => "running",
            SubagentStatus::Tool => "tool",
            SubagentStatus::Completed => "completed",
            SubagentStatus::Cancelled => "cancelled",
            SubagentStatus::TurnBudgetExhausted => "turn_budget_exhausted",
            SubagentStatus::Failed => "failed",
        }
    }
}

impl From<crate::shared::domain::orchestration::TaskStatus> for SubagentStatus {
    fn from(status: crate::shared::domain::orchestration::TaskStatus) -> Self {
        use crate::shared::domain::orchestration::TaskStatus;
        match status {
            TaskStatus::Queued => SubagentStatus::Queued,
            TaskStatus::Running => SubagentStatus::Running,
            TaskStatus::Completed => SubagentStatus::Completed,
            TaskStatus::Cancelled => SubagentStatus::Cancelled,
            TaskStatus::TurnBudgetExhausted => SubagentStatus::TurnBudgetExhausted,
            TaskStatus::Failed => SubagentStatus::Failed,
        }
    }
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
/// {"event":"audio_level","rms":0.42}          // wait/recording mic level, throttled
/// ```
///
/// `UserInput` and `AgentOutput` are legacy internal-only variants retained for
/// the tracing hook and existing call sites; they carry no external contract
/// shape and the NDJSON sink deliberately skips them (the streamed `Token`
/// deltas and `Transcript` already cover that information for the UI).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkflowEvent {
    /// Prefix warm-up progress at session start (see `Agent::prewarm`).
    /// `state` is `warming` while the model loads and the prompt prefix
    /// prefills, then exactly one of `ready` / `skipped` / `failed`. Emitted
    /// BEFORE `Ready`, so a shell can show "warming up" during the one stretch
    /// where the child is alive but cannot yet listen.
    Warmup { state: String },
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
    /// Live mic input level during `wait`/`recording`, throttled by the emitter
    /// (not every poll produces one — see the whisper adapter's audio-level sink).
    AudioLevel { rms: f32 },
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

/// Minimum interval between emitted audio-level readings, regardless of how
/// often the caller's polling loop runs.
const AUDIO_LEVEL_MIN_INTERVAL_MS: u128 = 100;
/// Minimum change in RMS required to re-emit within the interval window —
/// cuts idle-silence chatter once the level has settled.
const AUDIO_LEVEL_MIN_DELTA: f32 = 0.02;

/// Throttles a raw per-poll RMS stream down to a UI-friendly cadence before
/// handing it to an arbitrary sink (e.g. an NDJSON writer producing
/// [`WorkflowEvent::AudioLevel`] lines). Lives in `pond-core` (not an
/// adapter crate) so both the whisper adapter (mic input, wait/recording
/// states) and the piper adapter (TTS output, speaking state) can share one
/// throttle implementation without adapters depending on each other.
pub struct ThrottledAudioLevelSink {
    inner: Box<dyn Fn(f32) + Send + Sync>,
    last_emit: Mutex<Option<std::time::Instant>>,
    last_value: Mutex<f32>,
}

impl ThrottledAudioLevelSink {
    pub fn new(inner: Box<dyn Fn(f32) + Send + Sync>) -> Self {
        Self {
            inner,
            last_emit: Mutex::new(None),
            last_value: Mutex::new(0.0),
        }
    }

    /// Feed one poll's RMS reading. Emits through `inner` only if enough time
    /// has passed since the last emission AND the value moved meaningfully —
    /// otherwise it's a no-op.
    pub fn maybe_emit(&self, rms: f32) {
        let now = std::time::Instant::now();
        let mut last_emit = self.last_emit.lock().unwrap();
        let mut last_value = self.last_value.lock().unwrap();

        let due = match *last_emit {
            None => true,
            Some(t) => now.duration_since(t).as_millis() >= AUDIO_LEVEL_MIN_INTERVAL_MS,
        };
        if !due || (rms - *last_value).abs() < AUDIO_LEVEL_MIN_DELTA {
            return;
        }
        *last_emit = Some(now);
        *last_value = rms;
        (self.inner)(rms);
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
    fn audio_level_line_matches_contract() {
        let ev = WorkflowEvent::AudioLevel { rms: 0.42 };
        assert_eq!(
            ev.to_ndjson().unwrap(),
            r#"{"event":"audio_level","rms":0.42}"#
        );
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

#[cfg(test)]
mod agent_request_scope_tests {
    use super::*;

    fn request(scope: ProfileScope) -> AgentRequest {
        AgentRequest {
            message: "what did I say about the boiler".to_string(),
            session_id: "s1".to_string(),
            model_role: "chat".to_string(),
            images: Vec::new(),
            voice_mode: false,
            canvas_mode: false,
            profile_scope: scope,
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
        }
    }

    /// The scope must survive the trip from the API edge to the adapter. It is
    /// resolved once, at the edge, precisely so nothing downstream re-derives
    /// it -- a second resolution could disagree and the deeper one would win.
    #[test]
    fn the_scope_survives_a_serde_round_trip() {
        for scope in [
            ProfileScope::Owner("jerry".into()),
            ProfileScope::Household,
            ProfileScope::Guest,
        ] {
            let json = serde_json::to_string(&request(scope.clone())).unwrap();
            let back: AgentRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(back.profile_scope, scope);
        }
    }

    /// A request serialized before this field existed must still deserialize,
    /// and must land on the behaviour that release had -- unfiltered household
    /// access. Anything narrower would silently break stored payloads; there is
    /// nothing wider.
    #[test]
    fn a_payload_from_before_the_field_existed_still_deserializes() {
        let legacy = r#"{"message":"hi","session_id":"s1","model_role":"chat"}"#;
        let parsed: AgentRequest = serde_json::from_str(legacy).expect("legacy payload must parse");
        assert_eq!(parsed.profile_scope, ProfileScope::Household);
    }

    /// PAI-6 P6. The renderer's spelling and the wire's spelling are the same
    /// string for every state.
    ///
    /// `main.rs` prints [`SubagentStatus::as_str`] while `routes.rs` serializes
    /// the value into the SSE frame, so the two are read by different clients
    /// and would drift silently. Iterating [`SubagentStatus::ALL`] rather than
    /// listing cases here is deliberate: a variant added without a spelling is
    /// caught by the `as_str` match arm, and a variant added without an `ALL`
    /// entry is caught by the length assertion below.
    #[test]
    fn the_wire_spelling_is_the_serialized_spelling() {
        assert_eq!(
            SubagentStatus::ALL.len(),
            7,
            "SubagentStatus::ALL no longer lists every variant, so every guard \
             that iterates it now skips one"
        );
        for status in SubagentStatus::ALL {
            let wire = serde_json::to_value(status).expect("status serializes");
            assert_eq!(
                wire.as_str(),
                Some(status.as_str()),
                "{status:?} serializes as {wire} but renders as `{}` -- a client \
                 branching on one of those two spellings can never be true",
                status.as_str()
            );
        }
    }

    /// A progress frame is about a CHILD, and carries no room for its
    /// conversation.
    ///
    /// PAI-6 invariant 4 and PAI-2's minimisation rule meet on this variant:
    /// `detail` is the only free-text field it has, and the whole design rests
    /// on that field being narrow. This pins the SHAPE -- four keys, `detail`
    /// absent when there is none -- so that a later change adding, say, a
    /// `text` or `thinking` key to the wire has to argue with a test.
    #[test]
    fn a_progress_frame_carries_four_fields_and_no_transcript() {
        let event = AgentStreamEvent::SubagentProgress {
            task_id: "t1".into(),
            role: "researcher".into(),
            status: SubagentStatus::Tool,
            detail: Some("giap-weather__get_forecast".into()),
        };
        let json = serde_json::to_value(&event).expect("event serializes");
        let object = json.as_object().expect("an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["detail", "role", "status", "task_id", "type"],
            "the progress frame grew a field. Every field on it is visible to \
             the desktop and to any paired client, so a new one is a decision \
             about what a subagent may say about itself"
        );
        assert_eq!(object["type"], "subagent_progress");

        let quiet = AgentStreamEvent::SubagentProgress {
            task_id: "t1".into(),
            role: "researcher".into(),
            status: SubagentStatus::Queued,
            detail: None,
        };
        let json = serde_json::to_value(&quiet).expect("event serializes");
        assert!(
            json.get("detail").is_none(),
            "a frame with no detail must omit the key rather than send null"
        );
    }
}
