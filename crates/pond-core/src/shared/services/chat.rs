use crate::models::domain::message::{ChatMessage, Role};
use crate::models::ports::agent::Agent;
use crate::models::ports::provider::LlmProvider;
use crate::models::ports::speech_energy::SpeechEnergy;
use crate::models::ports::voice_input::{SpeculativeSignal, VoiceInput};
use crate::models::ports::voice_output::VoiceOutput;
use crate::models::ports::wake_word::StreamingWakeWordDetector;
use crate::models::services::context_compactor::ContextCompactor;
use crate::models::services::instant_activation::InstantActivation;
use crate::prompts::{SYSTEM_PROMPT, TITLE_GENERATION_PROMPT};
use crate::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use crate::security::ports::event_log::EventLog;
use crate::shared::domain::agent::{AgentRequest, AgentStreamEvent, WorkflowEvent, WorkflowState};
use crate::shared::services::print_output::PrintOutput;
use crate::shared::services::stdin_input::StdinInput;
use crate::user_data::domain::session::SessionMessage;
use crate::user_data::ports::memory_extractor::MemoryExtractor;
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::ports::session_storage::SessionStorage;
use crate::user_data::services::memory_extraction::MemoryExtractionService;
use anyhow::Result;
use futures::StreamExt as _;
use std::io::{self, Write};
use std::sync::Arc;
use uuid::Uuid;

// ── Voice helpers ─────────────────────────────────────────────────────────────

/// Voice mode always uses the "chat" role. The LLM handles tool routing
/// natively via MCP — no pre-classification needed.
fn resolve_voice_role(_message: &str) -> String {
    "chat".to_string()
}

/// The bare tool name, stripping any MCP server prefix
/// (`giap-weather__get_current_weather` -> `get_current_weather`).
///
/// MCP tool ids are `<server>__<tool>` (see `parse_tool_name` in
/// pond-mcp-server). The egress tracker (#113) already records the bare name via
/// `set_current_tool`, so recording it here too keeps the activity feed
/// consistent between a tool's `tool.call` event and its `egress.http` events.
fn bare_tool_name(tool: &str) -> String {
    match tool.split_once("__") {
        Some((_, bare)) if !bare.is_empty() => bare.to_string(),
        _ => tool.to_string(),
    }
}

/// Voice-loop control classification for a raw transcript.
///
/// This is voice-CLI UI control (turn-taking / loop lifecycle), NOT a
/// tool/thinking classifier — these phrases are intercepted by `run_loop`
/// before the LLM ever sees them, so the no-keyword-classification rule
/// does not apply here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoiceCommand {
    /// Dismissal / sleep — return to wake-word mode, keep the loop alive.
    Dismissal,
    /// Hard exit — terminate the voice loop entirely.
    Exit,
    /// Ordinary utterance — hand it to the LLM.
    Normal,
}

/// Single source of truth for voice-loop control phrases. Used by BOTH
/// `run_loop`'s dismissal/exit gates AND the Q2-26 speculative gate, so the
/// keyword list never drifts between the two paths.
fn classify_voice_command(text: &str) -> VoiceCommand {
    let lower = text.trim().to_lowercase();
    let lower = lower.trim_end_matches(|c: char| c == '.' || c == '!');
    match lower {
        "bye" | "goodbye" | "good bye" | "dismissed" | "go to sleep" | "that's all"
        | "thats all" | "never mind" | "nevermind" | "stop" | "stop listening" => {
            VoiceCommand::Dismissal
        }
        "exit" | "quit" => VoiceCommand::Exit,
        _ => VoiceCommand::Normal,
    }
}

/// True if `text` is any voice-loop control phrase (dismissal OR hard exit)
/// that `run_loop` intercepts before the LLM sees it. Used by the Q2-26
/// speculative-chat path to avoid speculatively calling `chat_stream_once`
/// on a phrase that should never reach the LLM at all.
fn is_dismissal_or_exit_phrase(text: &str) -> bool {
    !matches!(classify_voice_command(text), VoiceCommand::Normal)
}

/// Truncate a tool-result payload to the NDJSON contract's 2000-char cap.
///
/// The contract specifies `content` is "truncated to 2000 chars". We count
/// Unicode scalar values (chars), not bytes, and cut on a char boundary so the
/// serialized JSON is always valid. Sub-cap payloads are returned unchanged.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

fn truncate_tool_result(mut content: String) -> String {
    // Find the byte offset of the (MAX+1)-th char. `char_indices().nth(N)`
    // early-exits after N+1 chars, so sub-cap payloads pay at most a bounded
    // scan and never a full `chars().count()`; the caller owns the String, so we
    // truncate in place with zero extra allocation on either branch.
    if let Some((byte_idx, _)) = content.char_indices().nth(TOOL_RESULT_MAX_CHARS) {
        content.truncate(byte_idx);
    }
    content
}

/// The tone that plays while the assistant is working, stopped exactly once.
///
/// It is the only feedback between the request and the answer, so it has two
/// jobs that pull in opposite directions: it must not stop early, and it must
/// never outlive the turn. A tone still playing after the assistant has gone
/// quiet is the worst of the failure modes — nothing is coming, and the sound
/// says something is.
///
/// A plain flag could not promise the second half. Every early return in the
/// turn — the `?` on a stream error most of all — skipped the stop and left
/// the tone thread running for the life of the process. Tying the stop to the
/// guard's scope means the compiler places it on paths nobody remembered to
/// write.
struct WorkingTone {
    output: Arc<dyn VoiceOutput>,
    stopped: bool,
}

impl WorkingTone {
    fn start(output: Arc<dyn VoiceOutput>) -> Self {
        output.start_thinking_tone();
        Self {
            output,
            stopped: false,
        }
    }

    /// Stop the tone now — the answer has started. Idempotent, because the
    /// first speakable sentence can arrive down several different paths.
    fn stop(&mut self) {
        if !self.stopped {
            self.stopped = true;
            self.output.stop_thinking_tone();
        }
    }
}

impl Drop for WorkingTone {
    fn drop(&mut self) {
        self.stop();
    }
}

// TTS text handling lives in the `pond-voice` leaf crate so the desktop shell
// (a separate cargo workspace, no pond-core) shares one implementation instead
// of carrying its own port. Re-exported below to keep call sites unchanged.
use pond_voice::text::{split_sentences, strip_markdown_for_speech};

/// Re-exported for backwards compatibility: this was `pub` here before the
/// move, so removing the path would be a breaking change to pond-core's API.
pub use pond_voice::text::filter_thinking;

/// Derive a short, deterministic session title from a user message.
///
/// Takes the first ~6 whitespace-separated words, trims surrounding
/// punctuation/quotes, and caps the result at 60 chars. Returns an empty
/// string when the input has no usable words (caller skips the update).
///
/// This is the no-LLM fallback used by `ChatService::ensure_session_title`
/// so sessions always get a human-readable title even when no provider is
/// attached (the common HTTP-handler path).
fn derive_title_from_text(text: &str) -> String {
    const MAX_WORDS: usize = 6;
    const MAX_CHARS: usize = 60;

    let cleaned = text.trim().trim_matches('"').trim_matches('\'');
    let title = cleaned
        .split_whitespace()
        .take(MAX_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    let title = title.trim().trim_matches('"').trim_matches('\'').trim();

    if title.chars().count() <= MAX_CHARS {
        title.to_string()
    } else {
        // Cap at MAX_CHARS on a char boundary and add an ellipsis.
        let truncated: String = title.chars().take(MAX_CHARS).collect();
        format!("{}…", truncated.trim_end())
    }
}

/// Domain Service: ChatService
///
/// Orchestrates the Wait → Listen → Thinking → Speak workflow loop.
/// Also persists messages to session storage for conversation history.
///
/// All inference is routed through the `Agent` port (GooseAdapter in production).
/// An optional `LlmProvider` may be attached solely for session title generation.
///
/// Input is abstracted via the `VoiceInput` port.  The default is
/// `StdinInput` (reads from stdin).  Override with `with_voice_input()`.
///
/// `Clone` is cheap — every field is an `Arc`, `Option<Arc>`, `String`, or a
/// small `Clone` value. The Q2-26 speculative path clones the service into a
/// spawned task so a provisional transcript can start inference early.
#[derive(Clone)]
pub struct ChatService {
    agent: Arc<dyn Agent>,
    provider: Option<Arc<dyn LlmProvider>>,
    voice_input: Arc<dyn VoiceInput>,
    voice_output: Arc<dyn VoiceOutput>,
    speech_energy: Arc<dyn SpeechEnergy>,
    /// Wake-word detector.  Defaults to `InstantActivation` (keyboard / stdin mode).
    /// All detectors implement `StreamingWakeWordDetector`; `run_loop` always calls
    /// `wait_for_activation_with_audio()` so captured command audio is available for
    /// the one-breath flow when the detector supports it.
    wake_word_detector: Arc<dyn StreamingWakeWordDetector>,
    session_id: String,
    session_storage: Arc<dyn SessionStorage>,
    /// System prompt sent to the LLM on every completion call.
    /// Defaults to `SYSTEM_PROMPT`; override with `with_system_prompt()`.
    system_prompt: String,
    /// Optional LLM-based context compactor.  When set, triggers at 80% of
    /// the context budget instead of falling straight to trim_to_budget.
    compactor: Option<ContextCompactor>,
    /// Optional Answer Reviewer — adversarial post-inference quality gate.
    answer_reviewer: Option<Arc<dyn crate::models::ports::answer_reviewer::AnswerReviewer>>,
    /// Optional memory extraction pipeline. When all three are set,
    /// `persist_assistant_turn` spawns extraction automatically so handlers
    /// cannot accidentally omit it.
    memory_extractor: Option<Arc<dyn MemoryExtractor>>,
    memory_extraction_service: Option<Arc<MemoryExtractionService>>,
    memory_repo: Option<Arc<dyn MemoryRepository>>,
    /// Optional unified activity log. When set, `persist_assistant_turn` records
    /// one Agent event, one Inference event (when token usage is known), and one
    /// Tool event per tool call — so the activity feed reflects chat activity,
    /// not just Auth/Network. `None` in tests and the CLI path.
    event_log: Option<Arc<dyn EventLog>>,
    /// Optional workflow-event sink. When set (e.g. the `--json-events` NDJSON
    /// writer), `emit_event` forwards every event to it in addition to tracing.
    /// `None` is zero-cost — `emit_event` only ever traces. `Arc` keeps clone
    /// cheap so the Q2-26 speculative task (which clones the service) carries
    /// the same sink and streams `Token` events from the spawned job.
    event_sink: Option<WorkflowEventSink>,
    /// Whether `run_loop` prints its human-facing banners/prompts to stdout.
    /// Defaults to `true` (the interactive terminal experience). Set `false`
    /// in `--json-events` mode so stdout carries NOTHING but NDJSON lines —
    /// human diagnostics still go to stderr via `eprintln!`/tracing.
    stdout_diagnostics: bool,
    /// Optional per-turn telemetry sink. When set, confirmed voice turns
    /// record a `TurnMetrics` row exactly like the REST path does.
    telemetry: Option<Arc<dyn crate::security::ports::telemetry::TelemetryPort>>,
    /// Model identifier for telemetry rows (the CLI knows `--model`).
    model_name: Option<String>,
}

/// Result of one non-persisting agent stream: the streamed/spoken text plus
/// the usage and performance stats carried by the agent's `Done` event.
#[derive(Debug)]
struct TurnOutcome {
    text: String,
    usage: Option<crate::models::ports::provider::UsageStats>,
    stats: Option<crate::shared::domain::turn_stats::TurnStats>,
    total_latency_ms: u64,
}

struct BargeInWatch {
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for BargeInWatch {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// A pluggable workflow-event observer. `run_chat`'s `--json-events` mode wires
/// an NDJSON stdout writer here; the desktop shell parses those lines. Kept as a
/// bare `Fn` so `pond-core` stays framework-free.
pub type WorkflowEventSink = Arc<dyn Fn(&WorkflowEvent) + Send + Sync>;

impl ChatService {
    pub fn new(
        agent: Arc<dyn Agent>,
        session_id: String,
        session_storage: Arc<dyn SessionStorage>,
    ) -> Self {
        Self {
            agent,
            provider: None,
            voice_input: Arc::new(StdinInput::new()),
            voice_output: Arc::new(PrintOutput),
            speech_energy: Arc::new(crate::models::ports::speech_energy::NoEnergy),
            wake_word_detector: Arc::new(InstantActivation),
            session_id,
            session_storage,
            system_prompt: SYSTEM_PROMPT.to_string(),
            compactor: None,
            answer_reviewer: None,
            memory_extractor: None,
            memory_extraction_service: None,
            memory_repo: None,
            event_log: None,
            event_sink: None,
            stdout_diagnostics: true,
            telemetry: None,
            model_name: None,
        }
    }

    /// Attach the unified activity log so `persist_assistant_turn` records
    /// Agent / Inference / Tool events for each turn. Handlers that omit this
    /// simply record nothing — best-effort, never fatal.
    pub fn with_event_log(mut self, event_log: Arc<dyn EventLog>) -> Self {
        self.event_log = Some(event_log);
        self
    }

    /// Attach a telemetry sink so confirmed voice turns record `TurnMetrics`.
    pub fn with_telemetry(
        mut self,
        telemetry: Arc<dyn crate::security::ports::telemetry::TelemetryPort>,
    ) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    /// Set the model identifier used in telemetry rows.
    pub fn with_model_name(mut self, model_name: impl Into<String>) -> Self {
        self.model_name = Some(model_name.into());
        self
    }

    /// Attach the memory extraction pipeline so `persist_assistant_turn`
    /// automatically triggers extraction. Handlers that omit this call simply
    /// skip extraction — no silent data loss, no handler-level boilerplate.
    pub fn with_memory_extraction(
        mut self,
        extractor: Arc<dyn MemoryExtractor>,
        service: Arc<MemoryExtractionService>,
        repo: Arc<dyn MemoryRepository>,
    ) -> Self {
        self.memory_extractor = Some(extractor);
        self.memory_extraction_service = Some(service);
        self.memory_repo = Some(repo);
        self
    }

    /// Attach an Answer Reviewer for post-inference adversarial quality review.
    pub fn with_answer_reviewer(
        mut self,
        reviewer: Arc<dyn crate::models::ports::answer_reviewer::AnswerReviewer>,
    ) -> Self {
        self.answer_reviewer = Some(reviewer);
        self
    }

    /// Access the LLM provider (if set) for constructing tool agents etc.
    pub fn provider_ref(&self) -> &Option<Arc<dyn LlmProvider>> {
        &self.provider
    }

    /// Attach a real LLM provider. When set, `chat_once` calls the provider
    /// with the full conversation history instead of the echo agent.
    pub fn with_provider(mut self, provider: Arc<dyn LlmProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Override the input source.  Defaults to `StdinInput`.
    pub fn with_voice_input(mut self, input: Arc<dyn VoiceInput>) -> Self {
        self.voice_input = input;
        self
    }

    /// Override the voice output.  Defaults to `PrintOutput` (stdout).
    pub fn with_voice_output(mut self, output: Arc<dyn VoiceOutput>) -> Self {
        self.voice_output = output;
        self
    }

    pub fn with_speech_energy(mut self, energy: Arc<dyn SpeechEnergy>) -> Self {
        self.speech_energy = energy;
        self
    }

    fn watch_for_barge_in(&self) -> Option<BargeInWatch> {
        use pond_voice::barge;

        // Nothing to listen with. Starting the poll anyway would wake a task
        // ten times a second for the length of every reply to be told there
        // is no microphone, which it already knows.
        if self.speech_energy.is_inert() {
            return None;
        }

        let energy = self.speech_energy.clone();
        let voice_output = self.voice_output.clone();

        let handle = tokio::spawn(async move {
            let mut gate = barge::BargeIn::while_speaking();
            let poll = std::time::Duration::from_millis(barge::POLL_MS);
            let mut warned_inert = false;

            loop {
                tokio::time::sleep(poll).await;

                let Some(rms) = energy.recent_rms(barge::WINDOW_MS) else {
                    gate.reset();
                    if !warned_inert {
                        tracing::debug!("barge-in inert: the microphone is not open for this turn");
                        warned_inert = true;
                    }
                    continue;
                };

                if gate.on_rms(rms) {
                    tracing::debug!(rms, "barge-in: the user spoke over the reply");
                    voice_output.stop_speaking();
                    return;
                }
            }
        });

        Some(BargeInWatch { handle })
    }

    /// Set the wake-word detector.  Defaults to `InstantActivation` (no wait).
    ///
    /// All detectors implement `StreamingWakeWordDetector`.  `run_loop` always calls
    /// `wait_for_activation_with_audio()`, so detectors that capture command audio
    /// (e.g. `WhisperKeywordDetector`) enable the one-breath flow automatically.
    pub fn with_wake_word_detector(mut self, detector: Arc<dyn StreamingWakeWordDetector>) -> Self {
        self.wake_word_detector = detector;
        self
    }

    /// Override the system prompt sent to the LLM.
    ///
    /// Use `pond_core::prompts::build_system_prompt()` to build a personalised
    /// prompt from `Settings`.  The default is the static `SYSTEM_PROMPT` constant.
    pub fn with_system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = prompt;
        self
    }

    /// Enable LLM-based context compaction.
    ///
    /// When set, `chat_once` will summarise older history while preserving the
    /// most recent turns whenever the conversation exceeds 80% of the context
    /// limit, instead of simply dropping old messages via `trim_to_budget`.
    pub fn with_context_compactor(mut self, compactor: ContextCompactor) -> Self {
        self.compactor = Some(compactor);
        self
    }

    /// Attach a workflow-event sink. Every `emit_event` call forwards to it in
    /// addition to tracing. Used by `pond-server chat --json-events` to write
    /// the NDJSON contract to stdout.
    ///
    /// The sink is invoked synchronously from the loop, so keep it cheap
    /// (a buffered line write + flush). It is cloned into the speculative task,
    /// so `Token` deltas from a speculative stream reach it too.
    pub fn with_event_sink(mut self, sink: WorkflowEventSink) -> Self {
        self.event_sink = Some(sink);
        self
    }

    /// Control whether `run_loop` prints human-facing banners/prompts to stdout.
    ///
    /// Pass `false` in `--json-events` mode so stdout carries only NDJSON lines;
    /// diagnostics continue to reach stderr (`eprintln!`) and tracing.
    pub fn with_stdout_diagnostics(mut self, enabled: bool) -> Self {
        self.stdout_diagnostics = enabled;
        self
    }

    /// Single-shot chat (useful for tests and non-interactive callers).
    ///
    /// All inference is routed through the `Agent` port (GooseAdapter in production).
    /// Goose manages conversation history and context compaction internally.
    /// Our `SessionStorage` is used only for the REST API's history/listing endpoints.
    pub async fn chat_once(&self, message: String) -> Result<String> {
        // Persist the user message first
        let user_msg = ChatMessage::user(message.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            user_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        // Always route through the Agent port (GooseAdapter in production), which manages
        // its own history, system prompt, and MCP tools internally.
        // The optional `self.provider` is kept solely for session title generation.
        let request = AgentRequest {
            message: message.clone(),
            session_id: self.session_id.clone(),
            model_role: resolve_voice_role(&message),
            images: Vec::new(),
            voice_mode: false,
            canvas_mode: false,
        };
        let response_text = self.agent.chat(request).await?.text;

        // Persist the assistant response
        let assistant_msg = ChatMessage::assistant(response_text.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            assistant_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        // Auto-generate a session title after the first exchange
        self.maybe_generate_title(&message, &response_text).await;

        Ok(response_text)
    }

    /// Auto-generate a title for the session after the very first exchange.
    ///
    /// Only fires when:
    ///   1. An `LlmProvider` is available (title generation needs an LLM)
    ///   2. The session has no title yet
    ///   3. This is the first user+assistant pair (2 messages total)
    ///
    /// The title is generated by sending the user message and assistant
    /// response to the LLM with `TITLE_GENERATION_PROMPT`, then storing
    /// the result via `session_storage.update_title()`.
    ///
    /// Failures are logged but never bubble up — title generation is
    /// best-effort and must never break the chat flow.
    async fn maybe_generate_title(&self, user_text: &str, assistant_text: &str) {
        // Only generate if we have an LLM provider
        let provider = match &self.provider {
            Some(p) => p,
            None => return,
        };

        // Check if session already has a title
        if let Ok(session) = self.session_storage.get_session(&self.session_id).await {
            if session.title.is_some() {
                return;
            }
        }

        // Check if this is the first exchange (exactly 2 messages: user + assistant).
        // Fetch only 3 to avoid loading the entire history just for a count check.
        if let Ok(msgs) = self
            .session_storage
            .get_messages_paginated(&self.session_id, 3, 0)
            .await
        {
            if msgs.len() != 2 {
                return;
            }
        }

        // Build context for the title generation LLM call
        let context = format!("User: {}\nAssistant: {}", user_text, assistant_text);
        let messages = vec![ChatMessage::user(&context)];

        match provider.complete(TITLE_GENERATION_PROMPT, messages).await {
            Ok(response) => {
                // Clean up: trim whitespace, remove quotes, limit length
                let title = response
                    .content
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .chars()
                    .take(80)
                    .collect::<String>();

                if !title.is_empty() {
                    if let Err(e) = self
                        .session_storage
                        .update_title(&self.session_id, title.clone())
                        .await
                    {
                        tracing::warn!(
                            session_id = %self.session_id,
                            error = %e,
                            "Failed to save LLM-generated session title"
                        );
                    } else {
                        tracing::info!(
                            session_id = %self.session_id,
                            title = %title,
                            "Auto-generated session title (LLM)"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %self.session_id,
                    error = %e,
                    "LLM title generation failed (non-fatal)"
                );
            }
        }
    }

    /// Persist the user side of a turn. Call before starting the agent stream
    /// so the message is saved even if the stream errors out.
    pub async fn persist_user_message(&self, message: &str) -> Result<()> {
        self.persist_user_message_with_images(message, Vec::new())
            .await
    }

    /// Persist the user side of a turn along with its image attachments
    /// (phase F2).
    ///
    /// The storage adapter decides where the bytes land; this service only has
    /// to stop dropping them. Attachment order is the order given.
    pub async fn persist_user_message_with_images(
        &self,
        message: &str,
        images: Vec<crate::models::domain::message::ImageAttachment>,
    ) -> Result<()> {
        let sm = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            ChatMessage::user_with_images(message, images),
        );
        self.session_storage
            .add_message(self.session_id.clone(), sm)
            .await?;
        Ok(())
    }

    /// Persist the assistant side of a turn. Call after the agent stream drains.
    /// `tool_results` is raw JSON strings (one per tool call, in call order).
    /// `usage` is `(prompt_tokens, completion_tokens)`; pass `None` if unavailable.
    pub async fn persist_assistant_turn(
        &self,
        tool_results: Vec<String>,
        assistant_text: &str,
        usage: Option<(u32, u32)>,
        model_name: Option<&str>,
    ) -> Result<()> {
        // Extract tool names for activity events before the loop below consumes
        // `tool_results`. Each entry is JSON carrying a `"tool"` field (built in
        // the chat handler); entries that don't parse are skipped, not fatal.
        let tool_names: Vec<String> = tool_results
            .iter()
            .filter_map(|s| {
                serde_json::from_str::<serde_json::Value>(s)
                    .ok()
                    .and_then(|v| v.get("tool").and_then(|t| t.as_str()).map(bare_tool_name))
            })
            .collect();

        for content in tool_results {
            let sm = SessionMessage::new(
                Uuid::new_v4().to_string(),
                self.session_id.clone(),
                ChatMessage::tool_result(content, String::new()),
            );
            self.session_storage
                .add_message(self.session_id.clone(), sm)
                .await?;
        }
        let sm = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            ChatMessage::assistant(assistant_text),
        )
        .with_token_counts(usage.map(|(p, _)| p), usage.map(|(_, c)| c));
        self.session_storage
            .add_message(self.session_id.clone(), sm)
            .await?;
        if let Some((prompt, completion)) = usage {
            if prompt > 0 || completion > 0 {
                let _ = self
                    .session_storage
                    .increment_usage(&self.session_id, prompt, completion, model_name)
                    .await;
            }
        }

        // Ensure the session has a title. Handlers build ChatService without a
        // provider, so the LLM-based `maybe_generate_title` never fires; without
        // this fallback every session stays `title = null`. This derives a
        // cheap, deterministic title from the first user message — no LLM call.
        self.ensure_session_title().await;

        // Record the turn's activity in the unified log (best-effort). Uses the
        // tool names already carried in `tool_results` and the token usage, so
        // the activity feed reflects chat activity, not just Auth/Network.
        self.record_turn_activity(&tool_names, usage, model_name)
            .await;

        Ok(())
    }

    /// Append the Agent / Inference / Tool events for one completed turn.
    ///
    /// Best-effort: a failed append is logged and swallowed — observability must
    /// never fail a turn (same contract as the auth-event and egress emitters).
    /// Metadata only (counts, model, tool names); message content already lives
    /// in `session_messages`, so these events stay `Internal`.
    async fn record_turn_activity(
        &self,
        tool_names: &[String],
        usage: Option<(u32, u32)>,
        model_name: Option<&str>,
    ) {
        let Some(event_log) = &self.event_log else {
            return;
        };

        let mut events = Vec::with_capacity(2 + tool_names.len());

        // The turn itself.
        events.push(
            Event::new(EventCategory::Agent, "agent.turn")
                .attr("tool_count", tool_names.len() as i64)
                .session(&self.session_id)
                .sensitivity(PrivacySensitivity::Internal),
        );

        // The inference, when token usage was reported (chat_stream path).
        if let Some((prompt, completion)) = usage {
            let mut ev = Event::new(EventCategory::Inference, "inference.completion")
                .attr("prompt_tokens", prompt as i64)
                .attr("completion_tokens", completion as i64)
                .session(&self.session_id)
                .sensitivity(PrivacySensitivity::Internal);
            if let Some(model) = model_name {
                ev = ev.attr("model", model);
            }
            events.push(ev);
        }

        // One per tool call.
        for tool in tool_names {
            events.push(
                Event::new(EventCategory::Tool, "tool.call")
                    .attr("tool", tool.as_str())
                    .session(&self.session_id)
                    .sensitivity(PrivacySensitivity::Internal),
            );
        }

        for event in events {
            if let Err(e) = event_log.append(event).await {
                tracing::warn!(error = %e, "failed to record turn activity event");
            }
        }
    }

    /// Set a session title if one is not already present, deriving it
    /// deterministically from the first user message (first ~6 words).
    ///
    /// This is the reliable fallback for the common path where no
    /// `LlmProvider` is attached (all HTTP handlers). It is best-effort:
    /// failures are logged, never bubbled, and it runs inside the
    /// persistence owner so the ChatService contract is preserved.
    async fn ensure_session_title(&self) {
        // Skip if a title already exists (either set here previously or by the
        // LLM path). A missing session is treated as "no title" — the update
        // below is a no-op for a non-existent row.
        if let Ok(session) = self.session_storage.get_session(&self.session_id).await {
            if session.title.is_some() {
                return;
            }
        }

        // Find the first user message to derive a title from.
        let first_user_text = match self
            .session_storage
            .get_messages_paginated(&self.session_id, 20, 0)
            .await
        {
            Ok(msgs) => msgs
                .into_iter()
                .find(|m| m.message.role == Role::User)
                .map(|m| m.message.content),
            Err(_) => None,
        };

        let Some(text) = first_user_text else {
            return;
        };

        let title = derive_title_from_text(&text);
        if title.is_empty() {
            return;
        }

        if let Err(e) = self
            .session_storage
            .update_title(&self.session_id, title.clone())
            .await
        {
            tracing::warn!(
                session_id = %self.session_id,
                error = %e,
                "Failed to save derived session title"
            );
        } else {
            tracing::info!(
                session_id = %self.session_id,
                title = %title,
                "Derived session title (deterministic fallback)"
            );
        }
    }

    /// Like `persist_assistant_turn` but also triggers memory extraction in a
    /// background task. Use this instead of the inline `tokio::spawn` pattern
    /// in HTTP handlers — the extraction cannot be accidentally omitted.
    pub async fn persist_assistant_turn_with_extraction(
        &self,
        tool_results: Vec<String>,
        assistant_text: &str,
        usage: Option<(u32, u32)>,
        model_name: Option<&str>,
        user_message: &str,
    ) -> Result<()> {
        self.persist_assistant_turn(tool_results, assistant_text, usage, model_name)
            .await?;

        if let (Some(ext), Some(svc), Some(repo)) = (
            self.memory_extractor.clone(),
            self.memory_extraction_service.clone(),
            self.memory_repo.clone(),
        ) {
            let user_msg = user_message.to_string();
            let asst_resp = assistant_text.to_string();
            let sid = self.session_id.clone();
            tokio::spawn(async move {
                svc.run(
                    ext.as_ref(),
                    repo.as_ref(),
                    &user_msg,
                    &asst_resp,
                    Some(&sid),
                )
                .await;
            });
        }

        Ok(())
    }

    /// Streaming chat — routes through the Agent, chunks TTS by sentence,
    /// AND persists the turn (user + assistant) to session storage.
    ///
    /// Differences from `chat_once`:
    /// - Calls `agent.chat_stream()` so text arrives token-by-token.
    /// - Speaks each completed sentence immediately (low-latency TTS).
    /// - Announces MCP tool calls with a short spoken phrase before execution.
    /// - Speaking happens *inside* this method; callers must NOT call
    ///   `voice_output.speak()` on the returned text.
    ///
    /// This is the persisting entry point used for a *confirmed* transcript.
    /// The Q2-26 speculative path must NOT call this — it calls
    /// `stream_response_inner` (no persistence) so a provisional transcript
    /// that later turns out wrong never lands a phantom turn in
    /// `pond_system.db`. `run_loop` persists the confirmed turn exactly once
    /// via `persist_confirmed_turn`.
    pub async fn chat_stream_once(&self, message: String) -> Result<String> {
        let fired_at = std::time::Instant::now();
        // Persist user message
        let user_msg = ChatMessage::user(message.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            user_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        // Stream + speak (no persistence inside).
        let outcome = self
            .stream_response_inner(message.clone(), fired_at)
            .await?;

        // Persist the assistant response and generate a title if needed.
        self.persist_assistant_response(&outcome.text, outcome.usage.as_ref())
            .await?;
        self.maybe_generate_title(&message, &outcome.text).await;
        self.record_turn_outcome(&outcome).await;
        Ok(outcome.text)
    }

    /// Streams a response through the Agent and speaks it, returning the full
    /// assistant text. Performs **no persistence** — the caller owns that.
    ///
    /// Used directly by the Q2-26 speculative path (which must be able to run
    /// on a provisional transcript and be discarded without side effects) and
    /// by `chat_stream_once` (which wraps it with persistence). `fired_at`
    /// timestamps when inference was kicked off, purely for TTFT telemetry.
    async fn stream_response_inner(
        &self,
        message: String,
        fired_at: std::time::Instant,
    ) -> Result<TurnOutcome> {
        // The LLM handles tool routing natively via MCP — no pre-classification needed.
        // This path is only ever reached via run_loop (the voice CLI loop),
        // so voice_mode is unconditionally true here — this gets the TTS-friendly
        // prompt and suppressed thinking that desktop's voice path already gets.
        let request = AgentRequest {
            message: message.clone(),
            session_id: self.session_id.clone(),
            model_role: "chat".to_string(),
            images: Vec::new(),
            voice_mode: true,
            canvas_mode: false,
        };

        // Clear any interrupt left over from the previous turn. Exactly once
        // per turn, before any speech: interrupt state is a property of the
        // turn, and clearing it per-utterance made a barge-in stop one sentence
        // and then let the rest of the reply play out.
        self.voice_output.begin_utterance();

        // ── Working tone ──────────────────────────────────────────────────
        // The one signal that the request was heard and is being worked on.
        // It starts before inference and runs until the first real sentence
        // is ready to speak.
        //
        // A spoken filler ("Skimming the surface.") used to play first. It
        // said the same thing the tone says, but took a full synthesis and
        // playback to say it — delaying the answer to announce that the
        // answer was coming. One signal, and the cheaper one.
        let mut tone = WorkingTone::start(self.voice_output.clone());

        let mut stream = self.agent.chat_stream(request).await?;
        let mut turn_usage: Option<crate::models::ports::provider::UsageStats> = None;
        let mut turn_stats: Option<crate::shared::domain::turn_stats::TurnStats> = None;
        let mut full_text = String::new();
        let mut sentence_buf = String::new();
        let mut spoken_first = false;
        let mut barge_in: Option<BargeInWatch> = None;
        let mut thought_filter = crate::models::services::thought_filter::ThoughtFilter::new();

        // Pipelined TTS: synthesize the next sentence while the current one plays.
        // `pending_audio` holds WAV bytes ready for playback while we synthesize ahead.
        // `first_sentence_spoken` gates clause-boundary speak() for the first sentence
        // so audio starts before sentence 2's synthesis completes.
        let mut pending_audio: Option<Vec<u8>> = None;
        let mut first_sentence_spoken = false;

        macro_rules! speak_pipelined {
            ($self:expr, $text:expr, $pending:expr, $first_spoken:expr) => {{
                let text = $text;
                if !*$first_spoken {
                    // First sentence: speak() with clause-boundary splitting so the
                    // user hears audio immediately rather than waiting for S2 synth.
                    *$first_spoken = true;
                    if let Err(e) = $self.voice_output.speak(&text).await {
                        tracing::warn!("TTS failed: {}", e);
                    }
                } else {
                    // Subsequent sentences: synthesize-ahead pipeline.
                    match $self.voice_output.synthesize(&text).await {
                        Ok(Some(new_wav)) => {
                            if let Some(prev) = $pending.take() {
                                if let Err(e) = $self.voice_output.play_audio(prev).await {
                                    tracing::warn!("TTS playback failed: {}", e);
                                }
                            }
                            *$pending = Some(new_wav);
                        }
                        _ => {
                            if let Some(prev) = $pending.take() {
                                if let Err(e) = $self.voice_output.play_audio(prev).await {
                                    tracing::warn!("TTS playback failed: {}", e);
                                }
                            }
                            if let Err(e) = $self.voice_output.speak(&text).await {
                                tracing::warn!("TTS failed: {}", e);
                            }
                        }
                    }
                }
            }};
        }

        while let Some(event_result) = stream.next().await {
            match event_result? {
                AgentStreamEvent::ToolCall { id, tool, .. } => {
                    // Visible to the UI, silent to the ear. Tool use is part
                    // of working on the request, and the working tone already
                    // says that — narrating each step ("Checking the
                    // weather.") interrupted the tone to repeat it, and on a
                    // multi-tool turn the user heard a stream of announcements
                    // before hearing a single word of the actual answer.
                    //
                    // The tone deliberately keeps playing here.
                    self.emit_event(WorkflowEvent::ToolCall {
                        tool: tool.clone(),
                        id: id.clone(),
                    });
                }
                AgentStreamEvent::Text { content } => {
                    // Strip all thinking/reasoning tags — not meant for TTS or transcript.
                    let content = thought_filter.push(&content);
                    if content.is_empty() {
                        continue;
                    }

                    if !spoken_first {
                        // From-fire, user-perceived first-text latency (includes
                        // quip/tone time). The engine-level TTFT arrives in the
                        // Done event's TurnStats and is what the summary prints.
                        tracing::debug!(
                            "[Q2-26 TTFT] {}ms from-fire",
                            fired_at.elapsed().as_millis()
                        );
                        self.emit_event(WorkflowEvent::StateChanged {
                            state: WorkflowState::Speak,
                        });
                        spoken_first = true;
                    }
                    // Stream the (thought-filtered) delta to the event sink so the
                    // desktop caption feed updates token-by-token. Emitted before
                    // buffering so a partial that never completes a sentence still
                    // reaches the UI.
                    self.emit_event(WorkflowEvent::Token {
                        content: content.clone(),
                    });
                    full_text.push_str(&content);
                    sentence_buf.push_str(&content);

                    let (sentences, remainder) = split_sentences(&sentence_buf);
                    sentence_buf = remainder;
                    for sentence in sentences {
                        let spoken = strip_markdown_for_speech(&sentence);
                        if spoken.is_empty() {
                            continue;
                        }
                        tone.stop();
                        // Start barge-in mic monitoring before first TTS playback.
                        // If the user speaks during TTS, the listener sets the
                        // interrupt flag and playback stops immediately.
                        if barge_in.is_none() {
                            barge_in = self.watch_for_barge_in();
                        }
                        speak_pipelined!(
                            self,
                            spoken,
                            &mut pending_audio,
                            &mut first_sentence_spoken
                        );
                    }
                }
                AgentStreamEvent::Done { usage, stats, .. } => {
                    turn_usage = usage;
                    turn_stats = stats;
                    // Flush any tail held back by the thought filter. Also append to
                    // full_text so the returned + persisted message includes the
                    // withheld lookahead bytes — otherwise the tail is spoken but
                    // dropped from history. (#153)
                    let tail = thought_filter.flush();
                    if !tail.is_empty() {
                        // Emit the tail as a Token too, so the desktop caption/
                        // transcript (built solely from `Token` events) matches the
                        // text that is spoken and persisted — otherwise the UI bubble
                        // ends short of the reply. Mirror the streamed-delta path:
                        // ensure Speak has fired first so Token never precedes it.
                        if !spoken_first {
                            self.emit_event(WorkflowEvent::StateChanged {
                                state: WorkflowState::Speak,
                            });
                            spoken_first = true;
                        }
                        self.emit_event(WorkflowEvent::Token {
                            content: tail.clone(),
                        });
                        full_text.push_str(&tail);
                        sentence_buf.push_str(&tail);
                    }
                    // Flush any remaining buffer
                    let remainder = sentence_buf.trim().to_string();
                    if !remainder.is_empty() {
                        let spoken = strip_markdown_for_speech(&remainder);
                        if !spoken.is_empty() {
                            tone.stop();
                            speak_pipelined!(
                                self,
                                spoken,
                                &mut pending_audio,
                                &mut first_sentence_spoken
                            );
                        }
                    }
                    sentence_buf.clear();
                    break;
                }
                AgentStreamEvent::Error { content } => {
                    return Err(anyhow::anyhow!("Agent stream error: {}", content));
                }
                AgentStreamEvent::ToolResult { id, tool, content } => {
                    // Surface the tool result to the event sink (NDJSON).
                    // Not spoken — informational only. Truncate to the contract's
                    // 2000-char cap so a huge tool payload cannot bloat one line.
                    let content = truncate_tool_result(content);
                    self.emit_event(WorkflowEvent::ToolResult { tool, id, content });
                }
                AgentStreamEvent::Status { .. }
                | AgentStreamEvent::Thinking { .. }
                | AgentStreamEvent::ReviewStatus { .. }
                | AgentStreamEvent::ReviewRevision { .. }
                // The engine's cap message itself arrives as Text and IS spoken;
                // this structured marker is for clients that can offer a
                // continue affordance, which a voice turn cannot.
                | AgentStreamEvent::TurnLimitReached { .. } => {
                    // Not spoken during streaming — informational only
                }
            }
        }

        // Play any remaining synthesized audio
        if let Some(last) = pending_audio.take() {
            if let Err(e) = self.voice_output.play_audio(last).await {
                tracing::warn!("TTS final playback failed: {}", e);
            }
        }

        // Flush thought filter tail if stream ended without Done. Same as the Done
        // branch, the tail must also reach full_text so the persisted message is
        // complete on this path too — the second, previously-unfixed flush. (#153)
        let tail = thought_filter.flush();
        if !tail.is_empty() {
            // Emit the tail as a Token too (see the Done branch): keep the caption
            // feed in sync with the spoken/persisted text on the no-Done path.
            if !spoken_first {
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Speak,
                });
            }
            self.emit_event(WorkflowEvent::Token {
                content: tail.clone(),
            });
            full_text.push_str(&tail);
            sentence_buf.push_str(&tail);
        }
        // Flush anything left if stream ended without Done
        let remainder = sentence_buf.trim().to_string();
        if !remainder.is_empty() {
            let spoken = strip_markdown_for_speech(&remainder);
            if !spoken.is_empty() {
                tone.stop();
                if let Err(e) = self.voice_output.speak(&spoken).await {
                    tracing::warn!("TTS final flush failed: {}", e);
                }
            }
        }

        // A turn that produced no speakable text still has to stop the tone.
        tone.stop();

        // Stop watching the microphone: all TTS for this turn is done, so
        // there is nothing left to interrupt. Dropping the watch is what stops
        // it, hence the explicit `drop` rather than letting it fall out of
        // scope — the ordering relative to the tone above is deliberate.
        drop(barge_in.take());

        // No persistence here — the caller (chat_stream_once for a confirmed
        // transcript, or run_loop's persist_confirmed_turn for the reused
        // speculative result) owns writing this turn to session storage.
        Ok(TurnOutcome {
            text: full_text,
            usage: turn_usage,
            stats: turn_stats,
            total_latency_ms: fired_at.elapsed().as_millis() as u64,
        })
    }

    /// Record a completed turn's usage + performance: session token totals,
    /// an optional `TurnMetrics` row, and the console summary line. Failures
    /// are logged, never fatal — the reply was already delivered.
    async fn record_turn_outcome(&self, outcome: &TurnOutcome) {
        if let Some(usage) = &outcome.usage {
            if let Err(e) = self
                .session_storage
                .increment_usage(
                    &self.session_id,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    self.model_name.as_deref(),
                )
                .await
            {
                tracing::warn!("failed to increment session usage: {e}");
            }
        }
        if let (Some(telemetry), Some(stats)) = (&self.telemetry, &outcome.stats) {
            let turn_number = telemetry
                .get_turns(&self.session_id)
                .await
                .map(|v| v.len() as u32)
                .unwrap_or(0)
                + 1;
            let metrics = crate::security::domain::turn_metrics::TurnMetrics {
                session_id: self.session_id.clone(),
                turn_number,
                prompt_tokens: stats.prompt_tokens,
                completion_tokens: stats.completion_tokens,
                ttft_ms: stats.ttft_ms.unwrap_or(outcome.total_latency_ms),
                total_latency_ms: outcome.total_latency_ms,
                tool_name: None,
                tool_latency_ms: None,
                tool_cache_hit: None,
                context_utilization_pct: stats.context_pct().unwrap_or(0.0),
                model_name: self.model_name.clone().unwrap_or_default(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                prefill_ms: stats.prefill_ms,
                model_load_ms: stats.model_load_ms,
                decode_tok_per_sec: stats.decode_tok_per_sec,
                prefill_tok_per_sec: stats.prefill_tok_per_sec,
                context_limit_tokens: stats.context_limit_tokens,
                inference_count: Some(stats.inference_count),
            };
            if let Err(e) = telemetry.record_turn(metrics).await {
                tracing::debug!("failed to record voice turn metrics: {e}");
            }
        }
        self.print_turn_summary(outcome);
    }

    /// One clean console line per turn — the "inference = summaries" contract.
    /// Suppressed in `--json-events` mode (stdout is NDJSON-only there).
    fn print_turn_summary(&self, outcome: &TurnOutcome) {
        if !self.stdout_diagnostics {
            return;
        }
        let Some(stats) = &outcome.stats else {
            return;
        };
        let mut parts: Vec<String> = Vec::new();
        if let Some(ttft) = stats.ttft_ms {
            parts.push(format!("ttft {ttft}ms"));
        }
        match (stats.prefill_ms, stats.prefill_tok_per_sec) {
            (Some(prefill), Some(rate)) => parts.push(format!(
                "prefill {} tok in {:.1}s ({:.0} tok/s)",
                stats.prompt_tokens,
                prefill as f32 / 1000.0,
                rate
            )),
            _ => parts.push(format!("prompt {} tok", stats.prompt_tokens)),
        }
        if let (Some(decode), Some(rate)) = (stats.decode_ms, stats.decode_tok_per_sec) {
            parts.push(format!(
                "decode {} tok in {:.1}s ({:.1} tok/s)",
                stats.completion_tokens,
                decode as f32 / 1000.0,
                rate
            ));
        }
        if let (Some(used), Some(limit)) = (stats.context_used_tokens, stats.context_limit_tokens) {
            parts.push(format!(
                "ctx {used}/{limit} ({:.0}%)",
                stats.context_pct().unwrap_or(0.0)
            ));
        }
        if let Some(load) = stats.model_load_ms {
            if load > 0 {
                parts.push(format!("load {:.1}s", load as f32 / 1000.0));
            }
        }
        if stats.inference_count > 1 {
            parts.push(format!("{} inferences", stats.inference_count));
        }
        if !parts.is_empty() {
            println!("  [turn] {}", parts.join(" | "));
        }
    }

    /// Persist a single assistant message to session storage. Split out of
    /// `chat_stream_once` so the Q2-26 speculative path can persist the
    /// assistant turn *after* the transcript is confirmed, not during the
    /// speculative stream.
    async fn persist_assistant_response(
        &self,
        full_text: &str,
        usage: Option<&crate::models::ports::provider::UsageStats>,
    ) -> Result<()> {
        let assistant_msg = ChatMessage::assistant(full_text.to_string());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            assistant_msg,
        )
        .with_token_counts(
            usage.map(|u| u.prompt_tokens),
            usage.map(|u| u.completion_tokens),
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;
        Ok(())
    }

    /// Persist a confirmed voice turn (user message + assistant response) that
    /// was produced by a speculative stream, and generate a title if needed.
    ///
    /// The Q2-26 speculative path runs `stream_response_inner` (no
    /// persistence) on a *provisional* transcript. Once `run_loop` confirms
    /// the final transcript matches, it calls this to write exactly one turn —
    /// keyed to the confirmed transcript — preserving the single-source-of-
    /// truth invariant (`pond_system.db` is authoritative; ChatService is the
    /// sole persistence owner).
    async fn persist_confirmed_turn(
        &self,
        confirmed_message: &str,
        response_text: &str,
        usage: Option<&crate::models::ports::provider::UsageStats>,
    ) -> Result<()> {
        let user_msg = ChatMessage::user(confirmed_message.to_string());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            user_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        self.persist_assistant_response(response_text, usage)
            .await?;
        // Prefer an LLM-summarized title when a provider is attached; otherwise
        // (the live GooseAdapter voice path builds ChatService WITHOUT a
        // provider) fall back to the deterministic first-message-derived title
        // so the chat sidebar shows a readable topic, never a raw session id.
        // `ensure_session_title` is guarded on `title.is_none()`, so it no-ops
        // when the LLM path already set one.
        self.maybe_generate_title(confirmed_message, response_text)
            .await;
        self.ensure_session_title().await;
        Ok(())
    }

    /// Q2-26: listen for the next utterance while *speculatively* starting the
    /// LLM response as soon as a provisional transcript is available, to hide
    /// whisper + first-token latency inside the end-of-speech silence wait.
    ///
    /// Returns `(confirmed_transcript, speculative)` where `speculative`, when
    /// present, is a still-running job that streamed a response for the
    /// **provisional** transcript `spec_transcript`. The job runs
    /// `stream_response_inner`, which performs **no persistence** — so if the
    /// provisional transcript turns out wrong, discarding the job leaves no
    /// phantom turn in `pond_system.db`.
    ///
    /// The caller (`run_loop`) must:
    ///   - race the job against the wake-word interrupt (barge-in parity), and
    ///   - only commit (persist) the job's result if `spec_transcript` equals
    ///     the `confirmed_transcript`; otherwise discard it and process the
    ///     confirmed transcript through the normal persisting path.
    ///
    /// A `SpeculativeSignal::Ready` for an empty or dismissal/exit phrase never
    /// starts a job — those are intercepted by `run_loop` before the LLM sees
    /// them, and a speculative call would bypass that.
    #[allow(clippy::type_complexity)]
    async fn listen_with_speculative_chat(
        &self,
    ) -> Result<(
        Option<String>,
        Option<(String, tokio::task::JoinHandle<Result<TurnOutcome>>)>,
    )> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SpeculativeSignal>();
        let callback: Box<dyn Fn(SpeculativeSignal) + Send + Sync> = Box::new(move |signal| {
            let _ = tx.send(signal);
        });

        let listen_fut = self.voice_input.listen_with_speculative(callback);
        tokio::pin!(listen_fut);

        // `(spec_transcript, handle)` — the transcript the job was fired on is
        // retained so `run_loop` can confirm it matches the final transcript
        // before persisting anything.
        let mut speculative: Option<(String, tokio::task::JoinHandle<Result<TurnOutcome>>)> = None;

        loop {
            tokio::select! {
                result = &mut listen_fut => {
                    let transcript = result?;
                    return Ok((transcript, speculative.take()));
                }
                Some(signal) = rx.recv() => {
                    match signal {
                        SpeculativeSignal::Ready(text)
                            if !text.is_empty() && !is_dismissal_or_exit_phrase(&text) =>
                        {
                            // Fire the LLM early on the provisional transcript.
                            // NOTE: uses stream_response_inner (NO persistence),
                            // so an incorrect provisional transcript can be
                            // discarded without leaving a phantom turn.
                            let svc = self.clone();
                            let spec_text = text.clone();
                            let fired_at = std::time::Instant::now();
                            let handle = tokio::spawn(async move {
                                svc.stream_response_inner(spec_text, fired_at).await
                            });
                            speculative = Some((text, handle));
                        }
                        // Empty / dismissal / exit — let run_loop handle it normally.
                        SpeculativeSignal::Ready(_) => {}
                        SpeculativeSignal::Invalidated => {
                            // The provisional transcript covered a too-short
                            // clip (speech resumed). Abort the job and silence
                            // any audio it may have started. No persistence
                            // happened, so nothing to roll back.
                            if let Some((_, handle)) = speculative.take() {
                                handle.abort();
                                self.voice_output.stop_speaking();
                                self.voice_output.stop_thinking_tone();
                            }
                        }
                    }
                }
            }
        }
    }

    /// Run the interactive workflow loop.
    ///
    /// State machine:
    ///   Wait → Listen → Thinking → Speak → (back to Wait)
    ///
    /// Input is obtained via the `VoiceInput` port (stdin by default).
    pub async fn run_loop(&self) -> Result<()> {
        // First interaction always requires the wake word.
        // After that, conversational turn-taking: Goose listens for the user's
        // next turn directly after speaking, no wake word needed.
        // If the user doesn't speak (empty transcription), fall back to wake word.
        let mut first_turn = true;

        // Pre-captured input from a wake-word interrupt.  When set, the next
        // loop iteration skips the listen/wake-word phase and processes this
        // text directly — but still wrapped in `tokio::select!` so it remains
        // interruptible.
        let mut pending_input: Option<String> = None;

        // Human-facing stdout print, suppressed in `--json-events` mode so
        // stdout carries NOTHING but NDJSON lines. Diagnostics still reach
        // stderr (`eprintln!`) and tracing regardless of this flag.
        macro_rules! diag {
            ($($arg:tt)*) => {
                if self.stdout_diagnostics {
                    println!($($arg)*);
                }
            };
        }
        macro_rules! diag_inline {
            ($($arg:tt)*) => {
                if self.stdout_diagnostics {
                    print!($($arg)*);
                    let _ = io::stdout().flush();
                }
            };
        }

        loop {
            // `speculative`, when present, is a (provisional_transcript, job)
            // pair: an LLM response already streaming for a provisional
            // transcript (Q2-26). It is reused ONLY if the confirmed transcript
            // matches; otherwise it is discarded without persisting anything.
            let (input, speculative) = if let Some(text) = pending_input.take() {
                // Interrupt gave us pre-captured text — skip listen phase.
                // Emit events so the UI/state machine stays consistent.
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Listen,
                });
                (Some(text), None)
            } else if first_turn {
                // ── Wait for wake word ──
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Wait,
                });
                // A detector that does not wait has nothing to announce.
                let prompt = self.wake_word_detector.activation_prompt();
                if !prompt.is_empty() {
                    diag!("\n  {prompt}");
                }

                let activation = self
                    .wake_word_detector
                    .wait_for_activation_with_audio()
                    .await?;

                // ── Listen (one-breath or fresh recording) ──
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Listen,
                });
                diag_inline!("  {}", self.voice_input.prompt());

                if let Some(wav) = activation.captured_audio {
                    self.voice_input.prime_with_captured(wav);
                }

                self.listen_with_speculative_chat().await?
            } else {
                // ── Conversational turn — listen without wake word ──
                self.emit_event(WorkflowEvent::StateChanged {
                    state: WorkflowState::Listen,
                });
                diag!("\n  listening");

                self.listen_with_speculative_chat().await?
            };

            // Any early-return path (no speech / dismissal / exit) must abort a
            // live speculative job so it stops speaking and never persists.
            // Helper: aborts + silences the speculative job if one is running.
            let abort_speculative =
                |spec: Option<(String, tokio::task::JoinHandle<Result<TurnOutcome>>)>| {
                    if let Some((_, handle)) = spec {
                        handle.abort();
                        self.voice_output.stop_speaking();
                        self.voice_output.stop_thinking_tone();
                    }
                };

            let input = match input {
                None if first_turn => {
                    // Stdin EOF — exit the loop
                    abort_speculative(speculative);
                    self.emit_event(WorkflowEvent::Exit {
                        reason: "stdin_eof".to_string(),
                    });
                    diag!("\n  end of input");
                    break;
                }
                None => {
                    // Conversational mode, no speech — reset to wake word
                    abort_speculative(speculative);
                    diag!("  nothing heard");
                    first_turn = true;
                    continue;
                }
                Some(text) if text.is_empty() => {
                    // Empty transcription — fall back to wake word mode
                    abort_speculative(speculative);
                    if !first_turn {
                        diag!("  nothing heard");
                    }
                    first_turn = true;
                    continue;
                }
                Some(text) => text,
            };

            // ── Voice-loop control (dismissal / exit) — single source of truth ──
            match classify_voice_command(&input) {
                // Dismissal / sleep → speak farewell, return to wake word.
                VoiceCommand::Dismissal => {
                    abort_speculative(speculative);
                    let farewell = "Until next time. Just say my name when you need me.";
                    diag!("  {}", farewell);
                    if let Err(e) = self.voice_output.speak(farewell).await {
                        tracing::warn!("TTS farewell failed: {}", e);
                    }
                    first_turn = true;
                    continue;
                }
                // Hard exit → terminate the voice loop entirely.
                VoiceCommand::Exit => {
                    abort_speculative(speculative);
                    let farewell = "Goodbye! I'll be here whenever you need me.";
                    diag!("  {}", farewell);
                    if let Err(e) = self.voice_output.speak(farewell).await {
                        tracing::warn!("TTS farewell failed: {}", e);
                    }
                    self.emit_event(WorkflowEvent::Exit {
                        reason: "dismissed".to_string(),
                    });
                    break;
                }
                VoiceCommand::Normal => {}
            }

            // Conversation is active — subsequent turns skip the wake word
            first_turn = false;

            // Confirmed user utterance — surface to the event sink (NDJSON
            // `transcript`) before the LLM starts. Legacy UserInput retained
            // for the tracing hook.
            self.emit_event(WorkflowEvent::Transcript {
                text: input.clone(),
            });
            self.emit_event(WorkflowEvent::UserInput(input.clone()));

            // ── Thinking → Speak (streaming), with wake-word interrupt ──
            self.emit_event(WorkflowEvent::StateChanged {
                state: WorkflowState::Thinking,
            });

            // ── Q2-26 phantom-turn gate ─────────────────────────────────────
            // A speculative job (if any) streamed a response for a *provisional*
            // transcript with NO persistence. Reuse it ONLY if the confirmed
            // transcript matches; otherwise discard it and fall through to the
            // normal persisting path on the confirmed transcript. This
            // guarantees exactly ONE persisted turn per utterance, always keyed
            // to the confirmed transcript (single-source-of-truth invariant).
            let reusable_speculative = match speculative {
                Some((spec_transcript, handle)) if spec_transcript == input => Some(handle),
                Some((_, handle)) => {
                    // Mismatch: provisional transcript was wrong. Abort the job
                    // and silence any audio it started — nothing was persisted.
                    handle.abort();
                    self.voice_output.stop_speaking();
                    self.voice_output.stop_thinking_tone();
                    None
                }
                None => None,
            };

            // Race the agent response against the wake word detector.
            // If the user says the wake word during inference or TTS playback,
            // interrupt immediately: stop TTS, drop the stream, and process
            // the new speech as a fresh request.
            //
            // `chat_handle` is either the reusable speculative job (already
            // streaming, NO persistence) or a fresh non-persisting stream.
            // Either way it is raced against the wake interrupt identically, so
            // barge-in works the same. Persistence happens AFTER it completes,
            // via persist_confirmed_turn — so exactly one confirmed turn lands.
            let input_for_task = input.clone();
            let chat_handle = reusable_speculative.unwrap_or_else(|| {
                let svc = self.clone();
                let msg = input_for_task;
                let fired_at = std::time::Instant::now();
                tokio::spawn(async move { svc.stream_response_inner(msg, fired_at).await })
            });

            // ── InstantActivation race guard ─────────────────────────────────
            // The wake-word interrupt race is ONLY correct for detectors that
            // actually wait for real audio. `InstantActivation` (stdin /
            // --no-wake-word / whisper-load-failure fallback) resolves instantly
            // and would win the race before any turn could complete, aborting
            // EVERY turn. When the detector cannot interrupt, await the turn
            // directly — no race, no phantom abort. Real streaming detectors
            // keep the barge-in race below.
            if !self.wake_word_detector.supports_interruption() {
                let chat_result = chat_handle.await;
                if !self.finalize_confirmed_turn(chat_result, &input).await {
                    first_turn = true;
                }
                continue;
            }

            // Race the agent response against the wake word detector.
            // If the user says the wake word during inference or TTS playback,
            // interrupt immediately: stop TTS, drop the stream, and process
            // the new speech as a fresh request.
            //
            // `chat_handle` is either the reusable speculative job (already
            // streaming, NO persistence) or a fresh non-persisting stream.
            // Either way it is raced against the wake interrupt identically, so
            // barge-in works the same. Persistence happens AFTER it completes,
            // via persist_confirmed_turn — so exactly one confirmed turn lands.
            let wake_fut = self.wake_word_detector.wait_for_activation_with_audio();

            tokio::pin!(chat_handle);
            tokio::pin!(wake_fut);

            tokio::select! {
                chat_result = &mut chat_handle => {
                    // Normal completion — agent finished before any interrupt.
                    if !self.finalize_confirmed_turn(chat_result, &input).await {
                        first_turn = true;
                    }
                }
                wake_result = &mut wake_fut => {
                    // Wake word detected during inference/TTS — INTERRUPT
                    diag!("\n  interrupted");

                    // Stop any in-progress TTS playback and background listeners
                    self.voice_output.stop_speaking();
                    self.voice_output.stop_thinking_tone();

                    // `chat_handle` is a spawned task (fresh or reused speculative).
                    // Aborting it propagates the same drop-based cancellation into
                    // Goose's spawn_blocking inference, causing TokenAction::Stop
                    // within one token cycle. Crucially, the interrupted task ran
                    // `stream_response_inner` (NO persistence), so an interrupted
                    // response never leaves a partial turn in pond_system.db —
                    // persistence only happens on normal completion above.
                    // No TurnComplete is emitted either — the turn was never
                    // committed.
                    chat_handle.abort();

                    // Capture the user's new speech (wake word may include trailing audio).
                    // Instead of processing inline (which would be non-interruptible),
                    // stash the text in `pending_input` and `continue` the loop so
                    // the next iteration wraps it in tokio::select! again.
                    match wake_result {
                        Ok(activation) => {
                            self.emit_event(WorkflowEvent::StateChanged {
                                state: WorkflowState::Listen,
                            });
                            diag_inline!("  {}", self.voice_input.prompt());

                            if let Some(wav) = activation.captured_audio {
                                self.voice_input.prime_with_captured(wav);
                            }

                            match self.voice_input.listen().await {
                                Ok(Some(new_text)) if !new_text.is_empty() => {
                                    // Stash for the next loop iteration (interruptible path)
                                    pending_input = Some(new_text);
                                }
                                _ => {
                                    // No speech after interrupt — return to wake word mode
                                    diag!("  nothing heard");
                                    first_turn = true;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Wake word interrupt error: {}", e);
                            first_turn = true;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Finalize a completed (non-interrupted) chat turn: persist it exactly
    /// once, then emit the terminal events. Shared by both the raced and the
    /// non-raced (`supports_interruption() == false`) paths so persistence and
    /// event emission never drift between them.
    ///
    /// Returns `true` when the loop should stay in conversational mode, `false`
    /// on a stream/join error (the caller then resets to wake-word mode).
    ///
    /// On a stream/join error NOTHING is persisted. On a *persistence* failure the
    /// reply was already fully streamed and spoken to the user, so we do NOT reset
    /// to wake-word mode (that would kick the user out mid-conversation for a reply
    /// they just heard): we log + emit an `Error` for observability, SKIP
    /// `TurnComplete` (persistence is that event's contract — the turn is absent
    /// from history), and return `true` to keep the loop alive. `TurnComplete` is
    /// still emitted exactly once, and only when the turn actually persisted.
    async fn finalize_confirmed_turn(
        &self,
        chat_result: std::result::Result<Result<TurnOutcome>, tokio::task::JoinError>,
        input: &str,
    ) -> bool {
        match chat_result {
            Ok(Ok(outcome)) => {
                let response_text = outcome.text.clone();
                // Persist the confirmed turn exactly once (user + assistant),
                // keyed to the confirmed transcript.
                if let Err(e) = self
                    .persist_confirmed_turn(input, &response_text, outcome.usage.as_ref())
                    .await
                {
                    // Persistence failed (e.g. transient SQLITE_BUSY from serve +
                    // child both writing the WAL). The reply is already spoken, so
                    // surface the error but keep the conversation going — no
                    // TurnComplete (nothing landed in history), no wake-word reset.
                    tracing::warn!("Failed to persist confirmed turn: {}", e);
                    self.emit_event(WorkflowEvent::Error {
                        message: format!("failed to persist turn: {e}"),
                    });
                    return true;
                }
                self.record_turn_outcome(&outcome).await;
                self.emit_event(WorkflowEvent::AgentOutput(response_text));
                self.emit_event(WorkflowEvent::TurnComplete {
                    session_id: self.session_id.clone(),
                });
                true
            }
            Ok(Err(e)) => {
                eprintln!("  ❌ Error: {}", e);
                self.emit_event(WorkflowEvent::Error {
                    message: e.to_string(),
                });
                false
            }
            Err(join_err) => {
                eprintln!("  ❌ Error: {}", join_err);
                self.emit_event(WorkflowEvent::Error {
                    message: join_err.to_string(),
                });
                false
            }
        }
    }

    /// Emit a workflow event: trace it, then forward to the optional sink
    /// (e.g. the `--json-events` NDJSON writer). `None` sink is zero-cost.
    fn emit_event(&self, event: WorkflowEvent) {
        match &event {
            WorkflowEvent::StateChanged { state } => {
                tracing::debug!("Workflow state: {}", state);
            }
            WorkflowEvent::UserInput(text) => {
                tracing::debug!("User input: {}", text);
            }
            WorkflowEvent::AgentOutput(text) => {
                tracing::debug!("Agent output: {}", text);
            }
            WorkflowEvent::Exit { reason } => {
                tracing::debug!("Workflow exit requested: {}", reason);
            }
            WorkflowEvent::Ready { session_id } => {
                tracing::debug!(session_id = %session_id, "Voice session ready");
            }
            WorkflowEvent::Transcript { text } => {
                tracing::debug!("Transcript: {}", text);
            }
            WorkflowEvent::Token { .. } => {
                // High-frequency — do not trace per-token.
            }
            WorkflowEvent::ToolCall { tool, id } => {
                tracing::debug!(tool = %tool, id = %id, "Tool call");
            }
            WorkflowEvent::ToolResult { tool, id, .. } => {
                tracing::debug!(tool = %tool, id = %id, "Tool result");
            }
            WorkflowEvent::TurnComplete { session_id } => {
                tracing::debug!(session_id = %session_id, "Turn complete");
            }
            WorkflowEvent::Error { message } => {
                tracing::debug!("Workflow error: {}", message);
            }
        }

        if let Some(sink) = &self.event_sink {
            sink(&event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::mocks::mock_provider::MockProvider;
    use crate::shared::mocks::mock_agent::MockAgent;
    use crate::user_data::mocks::mock_session::InMemorySessionStorage;

    #[tokio::test]
    async fn chat_once_returns_echo() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        let result = service.chat_once("Hello!".to_string()).await.unwrap();
        assert_eq!(result, "Echo: Hello!");
    }

    // ── activity events (Agent / Inference / Tool) ───────────────────────

    use crate::security::domain::event::EventCategory;
    use crate::security::mocks::mock_event_log::MockEventLog;

    /// A tool_result JSON string in the shape the chat handler builds.
    fn tool_result(tool: &str) -> String {
        serde_json::json!({ "tool_call_id": "id", "tool": tool, "content": "ok" }).to_string()
    }

    #[test]
    fn bare_tool_name_strips_the_mcp_prefix() {
        assert_eq!(
            bare_tool_name("giap-weather__get_current_weather"),
            "get_current_weather"
        );
        assert_eq!(bare_tool_name("ext-filesystem__read_file"), "read_file");
        // No prefix — unchanged.
        assert_eq!(bare_tool_name("save_memory"), "save_memory");
        // Degenerate: trailing separator, keep the original rather than empty.
        assert_eq!(bare_tool_name("weird__"), "weird__");
    }

    async fn service_with_log(session: &str) -> (ChatService, Arc<MockEventLog>) {
        let storage = Arc::new(InMemorySessionStorage::new());
        storage.create_session(session.to_string()).await.unwrap();
        let log = Arc::new(MockEventLog::default());
        let service = ChatService::new(Arc::new(MockAgent::new()), session.to_string(), storage)
            .with_event_log(log.clone());
        (service, log)
    }

    #[tokio::test]
    async fn turn_emits_agent_inference_and_one_tool_event_each() {
        let (service, log) = service_with_log("sess-a").await;

        service
            .persist_assistant_turn(
                // Prefixed MCP ids as the stream delivers them; events record
                // the bare names.
                vec![
                    tool_result("giap-weather__get_current_weather"),
                    tool_result("giap-memory__save_memory"),
                ],
                "here you go",
                Some((120, 34)),
                Some("gemma-4-E2B-it-Q4_K_M"),
            )
            .await
            .unwrap();

        let events = log.all();
        let by_cat = |c: EventCategory| events.iter().filter(|e| e.category == c).count();
        assert_eq!(by_cat(EventCategory::Agent), 1);
        assert_eq!(by_cat(EventCategory::Inference), 1);
        assert_eq!(by_cat(EventCategory::Tool), 2);

        // Everything is Internal and carries the session id.
        assert!(events
            .iter()
            .all(|e| e.privacy_sensitivity == PrivacySensitivity::Internal
                && e.session_id.as_deref() == Some("sess-a")));

        let agent = events
            .iter()
            .find(|e| e.category == EventCategory::Agent)
            .unwrap();
        assert_eq!(agent.action, "agent.turn");
        assert_eq!(agent.attributes.get("tool_count"), Some(&2_i64.into()));

        let inference = events
            .iter()
            .find(|e| e.category == EventCategory::Inference)
            .unwrap();
        assert_eq!(
            inference.attributes.get("prompt_tokens"),
            Some(&120_i64.into())
        );
        assert_eq!(
            inference.attributes.get("completion_tokens"),
            Some(&34_i64.into())
        );
        assert_eq!(
            inference.attributes.get("model"),
            Some(&"gemma-4-E2B-it-Q4_K_M".into())
        );

        let tools: Vec<_> = events
            .iter()
            .filter(|e| e.category == EventCategory::Tool)
            .filter_map(|e| e.attributes.get("tool").cloned())
            .collect();
        assert!(tools.contains(&"get_current_weather".into()));
        assert!(tools.contains(&"save_memory".into()));
    }

    /// No token usage (the agent_chat_stream path) → an Agent event but no
    /// Inference event, since there is nothing to report.
    #[tokio::test]
    async fn turn_without_usage_emits_no_inference_event() {
        let (service, log) = service_with_log("sess-b").await;

        service
            .persist_assistant_turn(vec![], "hi", None, None)
            .await
            .unwrap();

        let events = log.all();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.category == EventCategory::Agent)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.category == EventCategory::Inference)
                .count(),
            0
        );
    }

    /// Without an event log attached, persistence still succeeds and records
    /// nothing — the feature is opt-in and never fatal.
    #[tokio::test]
    async fn turn_without_event_log_records_nothing_and_succeeds() {
        let storage = Arc::new(InMemorySessionStorage::new());
        storage.create_session("sess-c".to_string()).await.unwrap();
        let service = ChatService::new(Arc::new(MockAgent::new()), "sess-c".to_string(), storage);

        service
            .persist_assistant_turn(vec![tool_result("noop")], "ok", Some((1, 1)), None)
            .await
            .expect("persistence must not fail without an event log");
    }

    // ── truncate_tool_result (NDJSON 2000-char cap) ──────────────────────

    #[test]
    fn truncate_tool_result_leaves_sub_cap_payload_unchanged() {
        let s = "small result".to_string();
        assert_eq!(truncate_tool_result(s), "small result");
    }

    #[test]
    fn truncate_tool_result_returns_exactly_cap_chars_unchanged() {
        // Exactly TOOL_RESULT_MAX_CHARS chars — must not be truncated.
        let s = "x".repeat(TOOL_RESULT_MAX_CHARS);
        let out = truncate_tool_result(s.clone());
        assert_eq!(out.chars().count(), TOOL_RESULT_MAX_CHARS);
        assert_eq!(out, s);
    }

    #[test]
    fn truncate_tool_result_caps_oversized_payload_at_char_boundary() {
        let s = "y".repeat(TOOL_RESULT_MAX_CHARS + 500);
        let out = truncate_tool_result(s);
        assert_eq!(out.chars().count(), TOOL_RESULT_MAX_CHARS);
    }

    #[test]
    fn truncate_tool_result_cuts_on_utf8_boundary_not_mid_codepoint() {
        // Multi-byte chars straddling the cap must not corrupt the string.
        // "é" is 2 bytes; a naive byte cut at 2000 could split one.
        let s = "é".repeat(TOOL_RESULT_MAX_CHARS + 10);
        let out = truncate_tool_result(s);
        assert_eq!(out.chars().count(), TOOL_RESULT_MAX_CHARS);
        // Still valid UTF-8 (would panic on a mid-codepoint truncate).
        assert!(out.chars().all(|c| c == 'é'));
    }

    // ── derive_title_from_text / ensure_session_title (DEF-7) ────────────

    #[test]
    fn derive_title_takes_first_six_words() {
        let title = derive_title_from_text("What is the capital of France exactly?");
        assert_eq!(title, "What is the capital of France");
    }

    #[test]
    fn derive_title_trims_quotes_and_whitespace() {
        assert_eq!(derive_title_from_text("  \"hello world\"  "), "hello world");
        assert_eq!(derive_title_from_text("'single quoted'"), "single quoted");
    }

    #[test]
    fn derive_title_empty_for_blank_input() {
        assert_eq!(derive_title_from_text(""), "");
        assert_eq!(derive_title_from_text("   "), "");
    }

    #[test]
    fn derive_title_caps_long_single_word() {
        let long = "a".repeat(100);
        let title = derive_title_from_text(&long);
        // 60 chars + ellipsis
        assert!(title.chars().count() <= 61);
        assert!(title.ends_with('…'));
    }

    #[tokio::test]
    async fn persist_assistant_turn_sets_deterministic_title() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service
            .persist_user_message("How do I reset my password on the router?")
            .await
            .unwrap();
        service
            .persist_assistant_turn(vec![], "Here's how...", None, None)
            .await
            .unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        assert_eq!(session.title.as_deref(), Some("How do I reset my password"));
    }

    #[tokio::test]
    async fn persist_assistant_turn_preserves_existing_title() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "kept-title".to_string();
        storage.create_session(session_id.clone()).await.unwrap();
        storage
            .update_title(&session_id, "My custom title".to_string())
            .await
            .unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service.persist_user_message("hello there").await.unwrap();
        service
            .persist_assistant_turn(vec![], "hi", None, None)
            .await
            .unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        assert_eq!(session.title.as_deref(), Some("My custom title"));
    }

    // ── is_dismissal_or_exit_phrase / classify_voice_command ─────────────

    #[test]
    fn dismissal_phrases_are_detected() {
        assert_eq!(classify_voice_command("goodbye"), VoiceCommand::Dismissal);
        assert_eq!(classify_voice_command("Stop."), VoiceCommand::Dismissal);
        assert_eq!(
            classify_voice_command("  STOP LISTENING  "),
            VoiceCommand::Dismissal
        );
        assert!(is_dismissal_or_exit_phrase("goodbye"));
        assert!(is_dismissal_or_exit_phrase("Stop."));
    }

    #[test]
    fn exit_phrases_are_classified_as_exit() {
        assert_eq!(classify_voice_command("exit"), VoiceCommand::Exit);
        assert_eq!(classify_voice_command("quit!"), VoiceCommand::Exit);
        assert!(is_dismissal_or_exit_phrase("exit"));
        assert!(is_dismissal_or_exit_phrase("quit!"));
    }

    #[test]
    fn non_dismissal_text_is_not_flagged() {
        assert_eq!(
            classify_voice_command("what's the weather like"),
            VoiceCommand::Normal
        );
        assert_eq!(
            classify_voice_command("stop and think about this"),
            VoiceCommand::Normal
        );
        assert_eq!(classify_voice_command(""), VoiceCommand::Normal);
        assert!(!is_dismissal_or_exit_phrase("what's the weather like"));
        assert!(!is_dismissal_or_exit_phrase("stop and think about this"));
        assert!(!is_dismissal_or_exit_phrase(""));
    }

    // ── listen_with_speculative_chat (Q2-26) ─────────────────────────────

    /// Drives `listen_with_speculative`'s callback through a scripted
    /// sequence of signals (yielding briefly after each so the caller's
    /// `tokio::select!` loop gets a chance to react), then resolves with
    /// `final_transcript` — mirroring how `record_mono_f32_vad` behaves for
    /// a confirmed recording.
    struct ScriptedSpeculativeVoiceInput {
        signals: Vec<SpeculativeSignal>,
        final_transcript: Option<String>,
    }

    #[async_trait::async_trait]
    impl VoiceInput for ScriptedSpeculativeVoiceInput {
        async fn listen(&self) -> Result<Option<String>> {
            Ok(self.final_transcript.clone())
        }

        async fn listen_with_speculative(
            &self,
            on_speculative: Box<dyn Fn(SpeculativeSignal) + Send + Sync>,
        ) -> Result<Option<String>> {
            for sig in &self.signals {
                on_speculative(sig.clone());
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
            Ok(self.final_transcript.clone())
        }

        fn prompt(&self) -> &str {
            "> "
        }
    }

    #[tokio::test]
    async fn speculative_chat_runs_and_is_reused_when_confirmed() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let voice_input = Arc::new(ScriptedSpeculativeVoiceInput {
            signals: vec![SpeculativeSignal::Ready("hello".to_string())],
            final_transcript: Some("hello".to_string()),
        });

        let service = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(voice_input);

        let (transcript, speculative) = service.listen_with_speculative_chat().await.unwrap();
        assert_eq!(transcript, Some("hello".to_string()));

        let (spec_transcript, handle) = speculative
            .expect("confirmed, non-dismissal transcript must yield a speculative handle");
        assert_eq!(
            spec_transcript, "hello",
            "the handle must carry the provisional transcript for the confirm-vs-spec gate"
        );
        let response = handle.await.unwrap().unwrap();
        assert_eq!(response.text, "Echo: hello");

        // The speculative stream persists NOTHING — the run_loop gate owns
        // persistence after confirmation. Storage must still be empty here.
        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert!(
            msgs.is_empty(),
            "speculative stream must not persist; found {} messages",
            msgs.len()
        );
    }

    #[tokio::test]
    async fn speculative_chat_invalidated_on_resumed_speech_yields_no_handle() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let voice_input = Arc::new(ScriptedSpeculativeVoiceInput {
            signals: vec![
                SpeculativeSignal::Ready("hello".to_string()),
                SpeculativeSignal::Invalidated,
            ],
            final_transcript: Some("hello there".to_string()),
        });

        let service = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(voice_input);

        let (transcript, speculative) = service.listen_with_speculative_chat().await.unwrap();
        assert_eq!(transcript, Some("hello there".to_string()));
        assert!(
            speculative.is_none(),
            "a false pause must abort the speculative job, not hand it back"
        );
    }

    #[tokio::test]
    async fn speculative_chat_skips_dismissal_phrases() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let voice_input = Arc::new(ScriptedSpeculativeVoiceInput {
            signals: vec![SpeculativeSignal::Ready("goodbye".to_string())],
            final_transcript: Some("goodbye".to_string()),
        });

        let service = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(voice_input);

        let (transcript, speculative) = service.listen_with_speculative_chat().await.unwrap();
        assert_eq!(transcript, Some("goodbye".to_string()));
        assert!(
            speculative.is_none(),
            "dismissal phrases must never get a speculative chat call"
        );
    }

    // ── Phantom-turn persistence invariant (Q2-26, data integrity) ───────

    #[tokio::test]
    async fn confirmed_turn_persists_exactly_once_user_and_assistant() {
        // The persisting entry point (chat_stream_once) writes exactly two
        // messages: the user turn and the assistant turn. This is the ground
        // truth the speculative path must match — no more, no less.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service
            .chat_stream_once("what's the weather".to_string())
            .await
            .unwrap();

        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(
            msgs.len(),
            2,
            "exactly one user + one assistant message must persist per turn"
        );
    }

    #[tokio::test]
    async fn stream_response_inner_persists_nothing() {
        // The non-persisting core used by the speculative path must never
        // touch storage — that is what makes discarding a wrong provisional
        // transcript safe (no phantom turn).
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        let response = service
            .stream_response_inner("hello".to_string(), std::time::Instant::now())
            .await
            .unwrap();
        assert_eq!(response.text, "Echo: hello");

        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert!(
            msgs.is_empty(),
            "stream_response_inner must not persist; found {} messages",
            msgs.len()
        );
    }

    #[tokio::test]
    async fn persist_confirmed_turn_writes_confirmed_transcript_only() {
        // Simulates the run_loop gate: a speculative stream ran on a provisional
        // transcript ("weath"), but the CONFIRMED transcript ("what's the
        // weather") is what gets persisted — never the provisional one. Exactly
        // one user + one assistant message, keyed to the confirmed text.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());

        // Provisional stream (no persistence).
        let response = service
            .stream_response_inner("weath".to_string(), std::time::Instant::now())
            .await
            .unwrap();

        // Confirmed transcript differs — commit the CONFIRMED one.
        service
            .persist_confirmed_turn(
                "what's the weather",
                &response.text,
                response.usage.as_ref(),
            )
            .await
            .unwrap();

        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(msgs.len(), 2, "exactly one user + one assistant turn");
        assert_eq!(
            msgs[0].message.content, "what's the weather",
            "the persisted user turn must be the CONFIRMED transcript, never the provisional one"
        );
    }

    #[tokio::test]
    async fn chat_stream_once_sets_voice_mode_true() {
        // chat_stream_once is only ever reached via run_loop, the voice CLI
        // loop — regression test for Q2-23 (voice_mode was hardcoded false,
        // silently dropping the TTS-friendly prompt + thinking suppression
        // that desktop's voice path already gets).
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent.clone(), session_id.clone(), storage.clone());
        service
            .chat_stream_once("Hello!".to_string())
            .await
            .unwrap();

        let request = agent.last_request().expect("agent should have been called");
        assert!(
            request.voice_mode,
            "chat_stream_once must set voice_mode: true so the agent renders \
             the <voice-mode> prompt section"
        );
    }

    #[tokio::test]
    async fn chat_persists_messages_to_storage() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service
            .chat_once("First message".to_string())
            .await
            .unwrap();

        let messages = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(messages.len(), 2); // User message + Assistant response
        assert_eq!(messages[0].message.content, "First message");
        assert!(messages[1].message.content.contains("First message"));
    }

    #[tokio::test]
    async fn chat_messages_persist_across_iterations() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());

        // First iteration
        service.chat_once("Message 1".to_string()).await.unwrap();

        // Second iteration
        service.chat_once("Message 2".to_string()).await.unwrap();

        let messages = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(messages.len(), 4); // 2 iterations × 2 messages each
        assert_eq!(messages[0].message.content, "Message 1");
        assert_eq!(messages[2].message.content, "Message 2");
    }

    #[tokio::test]
    async fn chat_with_provider_auto_generates_title() {
        let agent = Arc::new(MockAgent::new());
        let provider = Arc::new(MockProvider::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-test".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service =
            ChatService::new(agent, session_id.clone(), storage.clone()).with_provider(provider);

        // First message → triggers title generation
        service
            .chat_once("What is the weather?".to_string())
            .await
            .unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        // MockProvider returns "Mock response to: ..." which becomes the title
        assert!(
            session.title.is_some(),
            "Title should be auto-generated after first exchange"
        );
        let title = session.title.unwrap();
        assert!(!title.is_empty(), "Title should not be empty");
    }

    #[tokio::test]
    async fn title_not_regenerated_on_second_message() {
        let agent = Arc::new(MockAgent::new());
        let provider = Arc::new(MockProvider::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-stable".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service =
            ChatService::new(agent, session_id.clone(), storage.clone()).with_provider(provider);

        // First message → generates title
        service.chat_once("Hello".to_string()).await.unwrap();
        let first_title = storage
            .get_session(&session_id)
            .await
            .unwrap()
            .title
            .clone();

        // Second message → should NOT overwrite title
        service.chat_once("How are you?".to_string()).await.unwrap();
        let second_title = storage
            .get_session(&session_id)
            .await
            .unwrap()
            .title
            .clone();

        assert_eq!(
            first_title, second_title,
            "Title should not change after first generation"
        );
    }

    #[tokio::test]
    async fn no_title_without_provider() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "no-provider".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        // No provider → title stays None
        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service.chat_once("Hello".to_string()).await.unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        assert!(
            session.title.is_none(),
            "No title should be set without a provider"
        );
    }

    #[tokio::test]
    async fn with_voice_input_builder_compiles() {
        use crate::shared::services::stdin_input::StdinInput;
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let _service = ChatService::new(agent, session_id, storage)
            .with_voice_input(Arc::new(StdinInput::new()));
    }

    #[tokio::test]
    async fn with_voice_output_builder_compiles() {
        use crate::shared::services::print_output::PrintOutput;
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let _service =
            ChatService::new(agent, session_id, storage).with_voice_output(Arc::new(PrintOutput));
    }

    // ── InstantActivation race fix + event sink (terminal-voice-in-desktop) ──
    //
    // These are the CI-visible regressions for the class of bug where the
    // run_loop select! interrupt race fired before every turn could complete
    // whenever the wired detector resolved instantly (InstantActivation).
    // pond-server is check-only in CI, so this coverage lives in pond-core.

    /// Plays a scripted list of utterances, then `None` (EOF) so `run_loop`
    /// exits cleanly. Mirrors the pond-server pipeline test's double.
    struct ScriptedListenInput {
        script: std::sync::Mutex<std::collections::VecDeque<Option<String>>>,
    }

    impl ScriptedListenInput {
        fn new(lines: impl IntoIterator<Item = &'static str>) -> Self {
            let mut deque: std::collections::VecDeque<Option<String>> =
                lines.into_iter().map(|s| Some(s.to_string())).collect();
            deque.push_back(None); // trailing None → end-of-input (stdin EOF)
            Self {
                script: std::sync::Mutex::new(deque),
            }
        }
    }

    #[async_trait::async_trait]
    impl VoiceInput for ScriptedListenInput {
        async fn listen(&self) -> Result<Option<String>> {
            Ok(self.script.lock().unwrap().pop_front().flatten())
        }
    }

    /// Captures every string passed to `speak()`.
    #[derive(Default)]
    struct CapturingSpeak {
        spoken: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl VoiceOutput for CapturingSpeak {
        async fn speak(&self, text: &str) -> Result<()> {
            self.spoken.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    /// A thread-safe collector for the event sink.
    #[derive(Clone, Default)]
    struct EventCollector {
        events: Arc<std::sync::Mutex<Vec<WorkflowEvent>>>,
    }

    impl EventCollector {
        fn sink(&self) -> WorkflowEventSink {
            let events = self.events.clone();
            Arc::new(move |ev: &WorkflowEvent| {
                events.lock().unwrap().push(ev.clone());
            })
        }

        fn ndjson_events(&self) -> Vec<WorkflowEvent> {
            // Only the events that map to an NDJSON contract line.
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.to_ndjson().is_some())
                .cloned()
                .collect()
        }
    }

    /// Session storage whose `add_message` always fails — simulates a transient
    /// persist failure (e.g. SQLITE_BUSY from serve + child WAL contention).
    /// Everything else delegates to an in-memory store so setup/reads work.
    struct FailingAddStorage {
        inner: InMemorySessionStorage,
    }

    impl FailingAddStorage {
        fn new() -> Self {
            Self {
                inner: InMemorySessionStorage::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::user_data::ports::session_storage::SessionStorage for FailingAddStorage {
        async fn create_session(
            &self,
            session_id: String,
        ) -> std::result::Result<
            crate::user_data::domain::session::Session,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.create_session(session_id).await
        }
        async fn get_session(
            &self,
            session_id: &str,
        ) -> std::result::Result<
            crate::user_data::domain::session::Session,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.get_session(session_id).await
        }
        async fn add_message(
            &self,
            _session_id: String,
            _message: crate::user_data::domain::session::SessionMessage,
        ) -> std::result::Result<
            crate::user_data::domain::session::SessionMessage,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            Err(
                crate::user_data::ports::session_storage::SessionStorageError::StorageError(
                    "simulated SQLITE_BUSY".to_string(),
                ),
            )
        }
        async fn get_messages(
            &self,
            session_id: &str,
        ) -> std::result::Result<
            Vec<crate::user_data::domain::session::SessionMessage>,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.get_messages(session_id).await
        }
        async fn update_title(
            &self,
            session_id: &str,
            title: String,
        ) -> std::result::Result<(), crate::user_data::ports::session_storage::SessionStorageError>
        {
            self.inner.update_title(session_id, title).await
        }
        async fn delete_session(
            &self,
            session_id: &str,
        ) -> std::result::Result<(), crate::user_data::ports::session_storage::SessionStorageError>
        {
            self.inner.delete_session(session_id).await
        }
        async fn list_sessions(
            &self,
        ) -> std::result::Result<
            Vec<crate::user_data::domain::session::Session>,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.list_sessions().await
        }
        async fn get_messages_paginated(
            &self,
            session_id: &str,
            limit: usize,
            offset: usize,
        ) -> std::result::Result<
            Vec<crate::user_data::domain::session::SessionMessage>,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner
                .get_messages_paginated(session_id, limit, offset)
                .await
        }
        async fn get_recent_messages(
            &self,
            session_id: &str,
            limit: usize,
        ) -> std::result::Result<
            Vec<crate::user_data::domain::session::SessionMessage>,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.get_recent_messages(session_id, limit).await
        }
        async fn count_messages(
            &self,
            session_id: &str,
        ) -> std::result::Result<u64, crate::user_data::ports::session_storage::SessionStorageError>
        {
            self.inner.count_messages(session_id).await
        }
        async fn first_user_message(
            &self,
            session_id: &str,
        ) -> std::result::Result<
            Option<String>,
            crate::user_data::ports::session_storage::SessionStorageError,
        > {
            self.inner.first_user_message(session_id).await
        }
    }

    #[tokio::test]
    async fn run_loop_completes_turn_under_instant_activation() {
        // REGRESSION (InstantActivation race): with the default InstantActivation
        // detector (stdin / --no-wake-word), run_loop used to abort every turn
        // via the interrupt race. The turn must now complete and reach speak().
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "instant-race".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let output = Arc::new(CapturingSpeak::default());
        let input = Arc::new(ScriptedListenInput::new(["what is the capital of France"]));

        // Default wake-word detector is InstantActivation (supports_interruption=false).
        let svc = ChatService::new(agent, session_id, storage)
            .with_voice_input(input)
            .with_voice_output(output.clone());

        svc.run_loop().await.unwrap();

        let spoken = output.spoken.lock().unwrap().clone();
        assert!(
            spoken.iter().any(|s| s.contains("what is the capital")),
            "the turn must complete and reach speak(); got: {spoken:?}"
        );
    }

    #[tokio::test]
    async fn run_loop_persists_exactly_one_turn_under_instant_activation() {
        // The completed turn must persist exactly one user + one assistant
        // message — no phantom turns, no double-persist.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "instant-persist".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let input = Arc::new(ScriptedListenInput::new(["hello there"]));
        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()));

        svc.run_loop().await.unwrap();

        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(
            msgs.len(),
            2,
            "exactly one user + one assistant message must persist"
        );
    }

    #[tokio::test]
    async fn run_loop_derives_deterministic_session_title_without_provider() {
        // The live GooseAdapter voice path builds ChatService WITHOUT a
        // provider, so the LLM title path never runs. persist_confirmed_turn
        // must still derive a readable deterministic title from the first
        // utterance so the chat sidebar never shows a raw session id.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-voice-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let input = Arc::new(ScriptedListenInput::new([
            "how do I reset the router password",
        ]));
        // No .with_provider() — mirrors the live voice path.
        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()));

        svc.run_loop().await.unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        assert_eq!(
            session.title.as_deref(),
            Some("how do I reset the router"),
            "voice turn must derive a deterministic title without a provider"
        );
    }

    #[tokio::test]
    async fn run_loop_emits_contract_event_sequence_for_a_turn() {
        // Asserts the NDJSON event sequence for a single scripted turn:
        // state changes in order, transcript exactly once, tokens streamed,
        // turn_complete exactly once on completion, exit stdin_eof at the end.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "seq-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        let input = Arc::new(ScriptedListenInput::new(["tell me a joke"]));
        let svc = ChatService::new(agent, session_id.clone(), storage)
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_event_sink(collector.sink());

        svc.run_loop().await.unwrap();

        let events = collector.ndjson_events();

        // The turn's states must appear in order Wait → Listen → Thinking →
        // Speak. (After the turn the loop keeps cycling Wait/Listen while the
        // script drains to EOF, so assert the ordered prefix, not equality.)
        let states: Vec<WorkflowState> = events
            .iter()
            .filter_map(|e| match e {
                WorkflowEvent::StateChanged { state } => Some(*state),
                _ => None,
            })
            .collect();
        let turn_states = &states[..4.min(states.len())];
        assert_eq!(
            turn_states,
            [
                WorkflowState::Wait,
                WorkflowState::Listen,
                WorkflowState::Thinking,
                WorkflowState::Speak,
            ],
            "the turn's state transitions must be Wait→Listen→Thinking→Speak; got {states:?}"
        );
        // Thinking must precede Speak, and both occur exactly once for one turn.
        assert_eq!(
            states
                .iter()
                .filter(|s| **s == WorkflowState::Thinking)
                .count(),
            1,
            "exactly one Thinking for one turn"
        );
        assert_eq!(
            states
                .iter()
                .filter(|s| **s == WorkflowState::Speak)
                .count(),
            1,
            "exactly one Speak for one turn"
        );

        // Exactly one transcript, carrying the confirmed utterance.
        let transcripts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                WorkflowEvent::Transcript { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(transcripts, vec!["tell me a joke"], "one transcript event");

        // At least one token streamed.
        let token_count = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::Token { .. }))
            .count();
        assert!(
            token_count >= 1,
            "tokens must be streamed; got {token_count}"
        );

        // Exactly one turn_complete on completion.
        let turn_complete_count = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::TurnComplete { .. }))
            .count();
        assert_eq!(
            turn_complete_count, 1,
            "exactly one turn_complete on completion"
        );

        // Ready is NOT emitted by run_loop (the CLI emits it after model load);
        // exit(stdin_eof) is the last contract event.
        match events.last() {
            Some(WorkflowEvent::Exit { reason }) => assert_eq!(reason, "stdin_eof"),
            other => panic!("last event must be exit(stdin_eof); got {other:?}"),
        }
    }

    #[tokio::test]
    async fn thought_filter_tail_is_emitted_as_token_and_matches_persisted_text() {
        // REGRESSION (#153, event stream): when ThoughtFilter's lookahead holds
        // back the final bytes of a response, the flushed tail is appended to the
        // persisted/spoken text but was NOT emitted as a Token — so the desktop
        // caption (built solely from Token events) ended short of the reply.
        //
        // The response text here ends in a partial sentinel prefix ("<end_of_tu"),
        // which the filter withholds in Normal state until flush(). MockAgent
        // echoes the user message, so we drive the tail deterministically via the
        // utterance. The invariant we lock: the concatenation of all Token event
        // contents equals the persisted assistant message text.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "tail-token-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        let input = Arc::new(ScriptedListenInput::new(["the code is 42<end_of_tu"]));
        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_event_sink(collector.sink());

        svc.run_loop().await.unwrap();

        let events = collector.events.lock().unwrap().clone();

        // Concatenate every Token event's content (in emit order).
        let streamed: String = events
            .iter()
            .filter_map(|e| match e {
                WorkflowEvent::Token { content } => Some(content.as_str()),
                _ => None,
            })
            .collect();

        // The persisted assistant message is the ground truth for the reply text.
        let msgs = storage.get_messages(&session_id).await.unwrap();
        let assistant = msgs
            .iter()
            .find(|m| m.message.role == crate::models::domain::message::Role::Assistant)
            .expect("assistant turn must persist");

        assert_eq!(
            streamed, assistant.message.content,
            "the streamed Token events must reconstruct the full persisted reply, \
             including the ThoughtFilter tail flushed at stream end"
        );
        // Sanity: the reply's trailing bytes (withheld by the filter's lookahead
        // and released only at flush) reached the Token stream.
        assert!(
            streamed.ends_with("<end_of_tu"),
            "the withheld tail must reach the Token stream; got {streamed:?}"
        );
    }

    #[tokio::test]
    async fn finalize_persist_failure_keeps_loop_alive_and_skips_turn_complete() {
        // REGRESSION: a transient persist failure (SQLITE_BUSY from serve + child
        // WAL contention) used to hard-fail the turn — resetting to wake-word mode
        // mid-conversation for a reply the user already heard. Now finalize must:
        //   - emit an Error event (observability),
        //   - NOT emit TurnComplete (persistence is that event's contract),
        //   - return true so the loop stays in conversational mode.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(FailingAddStorage::new());
        let session_id = "persist-fail-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        let svc = ChatService::new(agent, session_id.clone(), storage)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_event_sink(collector.sink());

        // Drive finalize_confirmed_turn directly with a successfully-streamed reply
        // whose persistence will fail (add_message always errors).
        let stayed_conversational = svc
            .finalize_confirmed_turn(
                Ok(Ok(TurnOutcome {
                    text: "the answer is 42".to_string(),
                    usage: None,
                    stats: None,
                    total_latency_ms: 0,
                })),
                "what is the answer",
            )
            .await;

        assert!(
            stayed_conversational,
            "persist failure must NOT reset to wake-word mode — the reply was already spoken"
        );

        let events = collector.events.lock().unwrap().clone();

        let error_count = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::Error { .. }))
            .count();
        assert_eq!(error_count, 1, "exactly one Error event on persist failure");

        let turn_complete_count = events
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::TurnComplete { .. }))
            .count();
        assert_eq!(
            turn_complete_count, 0,
            "no TurnComplete when the turn failed to persist"
        );
    }

    /// A detector that supports interruption and interrupts immediately, to
    /// prove the interrupt path (interruptible detector) emits ZERO
    /// turn_complete and persists NOTHING.
    struct AlwaysInterruptDetector;

    #[async_trait::async_trait]
    impl StreamingWakeWordDetector for AlwaysInterruptDetector {
        async fn wait_for_activation_with_audio(
            &self,
        ) -> Result<crate::models::ports::wake_word::WakeWordActivation> {
            // First call (Wait phase): return quickly so the loop enters Listen.
            // The interrupt race then re-enters here and wins immediately.
            Ok(crate::models::ports::wake_word::WakeWordActivation {
                captured_audio: None,
            })
        }
        fn supports_interruption(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn interrupted_turn_persists_nothing_and_emits_no_turn_complete() {
        // With an interruptible detector that fires instantly, the in-flight
        // turn is aborted: NO persistence, NO turn_complete. This is the
        // exactly-once / none-on-interrupt invariant on the interrupt path.
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "interrupt-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let collector = EventCollector::default();
        // Listen returns text on the first turn, then None so the loop can end
        // after the interrupt stashes/discards.
        let input = Arc::new(ScriptedListenInput::new(["a long question"]));
        let svc = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_voice_input(input)
            .with_voice_output(Arc::new(CapturingSpeak::default()))
            .with_wake_word_detector(Arc::new(AlwaysInterruptDetector))
            .with_event_sink(collector.sink());

        // MockAgent sleeps 300ms before streaming, so the instant wake future
        // wins the race deterministically.
        svc.run_loop().await.unwrap();

        // Nothing persisted — the interrupted turn never committed.
        let msgs = storage.get_messages(&session_id).await.unwrap();
        assert!(
            msgs.is_empty(),
            "an interrupted turn must persist nothing; found {} messages",
            msgs.len()
        );

        // No turn_complete emitted for the interrupted turn.
        let turn_complete_count = collector
            .ndjson_events()
            .iter()
            .filter(|e| matches!(e, WorkflowEvent::TurnComplete { .. }))
            .count();
        assert_eq!(
            turn_complete_count, 0,
            "an interrupted turn must NOT emit turn_complete"
        );
    }
}
