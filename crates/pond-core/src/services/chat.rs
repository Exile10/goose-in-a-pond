use crate::domain::agent::{AgentRequest, AgentStreamEvent, WorkflowEvent, WorkflowState};
use crate::domain::message::ChatMessage;
use crate::domain::session::SessionMessage;
use crate::ports::agent::Agent;
use crate::ports::provider::LlmProvider;
use crate::ports::session_storage::SessionStorage;
use crate::ports::voice_input::VoiceInput;
use crate::ports::voice_output::VoiceOutput;
use crate::ports::wake_word::WakeWordDetector;
use crate::services::instant_activation::InstantActivation;
use crate::services::print_output::PrintOutput;
use crate::prompts::{SYSTEM_PROMPT, TITLE_GENERATION_PROMPT};
use crate::services::context_compactor::ContextCompactor;
use crate::services::stdin_input::StdinInput;
use anyhow::Result;
use futures::StreamExt as _;
use std::io::{self, Write};
use std::sync::Arc;
use uuid::Uuid;

// ── Voice helpers ─────────────────────────────────────────────────────────────

/// Classify a voice message into a model role string.
///
/// Voice mode defaults Chat-classified messages to "chat" as well; the role
/// is metadata for the GooseAdapter's model selection.
fn resolve_voice_role(message: &str) -> String {
    use crate::domain::model_role::ModelRole;
    use crate::services::request_classifier::classify_request;
    match classify_request(message) {
        ModelRole::Think => "think".to_string(),
        ModelRole::Task  => "task".to_string(),
        ModelRole::Chat  => "chat".to_string(),
    }
}

/// Human-readable announcement spoken while an MCP tool is executing.
fn tool_announcement(tool: &str) -> String {
    let name = tool.split("__").last().unwrap_or(tool);
    match name {
        "get_current_weather" | "get_weather" => "Let me check the weather.".to_string(),
        "get_devices" | "list_devices"        => "Checking your devices.".to_string(),
        "set_schedule" | "create_schedule"    => "Setting that up.".to_string(),
        "save_memory"                         => "Got it, I'll remember that.".to_string(),
        other => format!("Let me {}.", other.replace('_', " ")),
    }
}

/// Split completed sentences out of a text buffer.
///
/// Sentence boundaries: `.`, `?`, `!` followed by whitespace or end-of-string,
/// and bare newlines. Forces a flush at 250 characters to handle code blocks
/// or long lists without sentence punctuation.
///
/// Returns `(sentences_to_speak, remaining_buffer)`.
fn split_sentences(text: &str) -> (Vec<String>, String) {
    const MAX_BUF: usize = 250;
    let mut sentences: Vec<String> = Vec::new();
    let mut remainder = text.to_string();

    loop {
        // Force-flush at max buffer: break at last space within the limit
        if remainder.len() > MAX_BUF {
            if let Some(split_at) = remainder[..MAX_BUF].rfind(' ') {
                sentences.push(remainder[..split_at].to_string());
                remainder = remainder[split_at + 1..].to_string();
                continue;
            }
        }

        let mut found = false;
        let chars: Vec<(usize, char)> = remainder.char_indices().collect();
        for (idx, (i, ch)) in chars.iter().enumerate() {
            if matches!(ch, '.' | '?' | '!') {
                let next = i + ch.len_utf8();
                let after = &remainder[next..];
                if after.is_empty() || after.starts_with(' ') || after.starts_with('\n') {
                    sentences.push(remainder[..next].to_string());
                    remainder = after.trim_start_matches(|c: char| c == ' ' || c == '\n').to_string();
                    found = true;
                    break;
                }
            } else if *ch == '\n' {
                // Newline is its own boundary
                let chunk = remainder[..*i].trim().to_string();
                if !chunk.is_empty() {
                    sentences.push(chunk);
                }
                let next = i + 1;
                remainder = remainder[next..].to_string();
                found = true;
                let _ = idx; // suppress unused warning
                break;
            }
        }

        if !found {
            break;
        }
    }

    (sentences, remainder)
}

/// Convert a markdown string to plain text suitable for TTS.
///
/// Handles:
/// - Code fences (``` / ~~~) — block skipped entirely
/// - Inline code (`…`) — backticks removed, content kept
/// - Bold / italic (`**`, `__`, `*`, `_`) — markers removed
/// - Headers (`#`, `##`, …) — `#` stripped, text kept
/// - Blockquotes (`> `) — `>` stripped, text kept
/// - Unordered lists (`- `, `* `, `+ `) — marker stripped, text kept
/// - Ordered lists (`1. `, `2. `, …) — marker stripped, text kept
/// - Horizontal rules (`---`, `***`, `___`) — line dropped
/// - Links (`[text](url)`) — url dropped, text kept
/// - Images (`![alt](url)`) — dropped entirely
/// - Strikethrough (`~~…~~`) — markers removed
fn strip_markdown_for_speech(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code_fence = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Code fence toggle — skip body of code blocks
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }

        // Horizontal rules: --- *** ___ (3+ of the same char, nothing else)
        if is_hr(trimmed) {
            continue;
        }

        // Strip structural prefix then inline markers
        let content = strip_line_prefix(trimmed);
        let content = strip_inline_md(content);
        let content = content.trim().to_string();
        if !content.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&content);
        }
    }

    out.trim().to_string()
}

fn is_hr(s: &str) -> bool {
    if s.len() < 3 {
        return false;
    }
    let first = s.chars().next().unwrap_or(' ');
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    s.chars().all(|c| c == first || c == ' ')
}

/// Strip leading structural markdown from a line (header `#`, blockquote `>`, list marker).
fn strip_line_prefix(line: &str) -> &str {
    // Headers: ### text → text
    if line.starts_with('#') {
        return line.trim_start_matches('#').trim_start();
    }
    // Blockquotes: > text
    if let Some(rest) = line.strip_prefix("> ").or_else(|| line.strip_prefix('>')) {
        return rest.trim_start();
    }
    // Unordered lists: - / * / +
    if let Some(rest) = line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
    {
        return rest;
    }
    // Ordered lists: 1. 2. 10. etc.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && bytes.get(i) == Some(&b'.') && bytes.get(i + 1) == Some(&b' ') {
        return &line[i + 2..];
    }
    line
}

/// Strip inline markdown markers from a string, handling bold, italic, code, links, images.
fn strip_inline_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Images: ![alt](url) → dropped
        if chars[i] == '!' && chars.get(i + 1) == Some(&'[') {
            if let Some((end, _)) = find_link(&chars, i + 1) {
                i = end;
                continue;
            }
        }

        // Links: [text](url) → text
        if chars[i] == '[' {
            if let Some((end, text)) = find_link(&chars, i) {
                out.push_str(&text);
                i = end;
                continue;
            }
        }

        // Strikethrough: ~~text~~
        if chars.get(i..i + 2) == Some(&['~', '~']) {
            if let Some(close) = find_marker_close(&chars, i + 2, &['~', '~']) {
                out.push_str(&chars[i + 2..close].iter().collect::<String>());
                i = close + 2;
                continue;
            }
        }

        // Bold: **text** or __text__
        if chars.get(i..i + 2) == Some(&['*', '*'])
            || chars.get(i..i + 2) == Some(&['_', '_'])
        {
            let marker = [chars[i], chars[i + 1]];
            if let Some(close) = find_marker_close(&chars, i + 2, &marker) {
                out.push_str(&chars[i + 2..close].iter().collect::<String>());
                i = close + 2;
                continue;
            }
        }

        // Italic: *text* or _text_
        if chars[i] == '*' || chars[i] == '_' {
            let marker = [chars[i]];
            if let Some(close) = find_marker_close(&chars, i + 1, &marker) {
                out.push_str(&chars[i + 1..close].iter().collect::<String>());
                i = close + 1;
                continue;
            }
        }

        // Inline code: `text`
        if chars[i] == '`' {
            if let Some(close) = find_marker_close(&chars, i + 1, &['`']) {
                out.push_str(&chars[i + 1..close].iter().collect::<String>());
                i = close + 1;
                continue;
            }
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

/// Find a `[text](url)` link starting at `start` (which points to `[`).
/// Returns `(end_index, link_text)` where `end_index` is one past the closing `)`.
fn find_link(chars: &[char], start: usize) -> Option<(usize, String)> {
    if chars.get(start) != Some(&'[') {
        return None;
    }
    // Find closing ]
    let mut depth = 0usize;
    let mut j = start;
    while j < chars.len() {
        match chars[j] {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    if j >= chars.len() {
        return None;
    }
    let text_end = j; // index of `]`
    // Must be followed by `(`
    if chars.get(j + 1) != Some(&'(') {
        return None;
    }
    // Find closing )
    let mut k = j + 2;
    let mut depth2 = 1usize;
    while k < chars.len() && depth2 > 0 {
        match chars[k] {
            '(' => depth2 += 1,
            ')' => depth2 -= 1,
            _ => {}
        }
        k += 1;
    }
    if depth2 != 0 {
        return None;
    }
    let link_text: String = chars[start + 1..text_end].iter().collect();
    Some((k, link_text))
}

/// Find the closing occurrence of `marker` in `chars` starting at `start`.
/// Returns the index where the marker begins (not one-past-end).
fn find_marker_close(chars: &[char], start: usize, marker: &[char]) -> Option<usize> {
    let mlen = marker.len();
    let limit = chars.len().saturating_sub(mlen - 1);
    for i in start..limit {
        if &chars[i..i + mlen] == marker {
            return Some(i);
        }
    }
    None
}

/// Strip `<think>…</think>` reasoning blocks from a streaming text chunk.
///
/// Models like Qwen3/QwQ/DeepSeek-R1 emit reasoning inside `<think>` tags before
/// their actual answer.  TTS should skip that content; only the visible answer
/// should be spoken.
///
/// `in_block` is the carry-over state from the previous chunk (we may be in the
/// middle of a block that started in an earlier event).
///
/// Returns `(visible_text, updated_in_block)`.
fn filter_thinking(chunk: &str, mut in_block: bool) -> (String, bool) {
    let mut visible = String::with_capacity(chunk.len());
    let mut rest = chunk;

    loop {
        if in_block {
            // Inside a think block — look for the closing tag.
            if let Some(end) = rest.find("</think>") {
                rest = &rest[end + "</think>".len()..];
                in_block = false;
            } else {
                // Entire remaining chunk is still inside the block — skip it all.
                break;
            }
        } else {
            // Outside a think block — look for the opening tag.
            if let Some(start) = rest.find("<think>") {
                visible.push_str(&rest[..start]);
                rest = &rest[start + "<think>".len()..];
                in_block = true;
            } else {
                // No more think blocks — everything remaining is visible.
                visible.push_str(rest);
                break;
            }
        }
    }

    (visible, in_block)
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
pub struct ChatService {
    agent: Arc<dyn Agent>,
    provider: Option<Arc<dyn LlmProvider>>,
    voice_input: Arc<dyn VoiceInput>,
    voice_output: Arc<dyn VoiceOutput>,
    wake_word_detector: Arc<dyn WakeWordDetector>,
    session_id: String,
    session_storage: Arc<dyn SessionStorage>,
    /// System prompt sent to the LLM on every completion call.
    /// Defaults to `SYSTEM_PROMPT`; override with `with_system_prompt()`.
    system_prompt: String,
    /// Optional LLM-based context compactor.  When set, triggers at 80% of
    /// the context budget instead of falling straight to trim_to_budget.
    compactor: Option<ContextCompactor>,
}

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
            wake_word_detector: Arc::new(InstantActivation),
            session_id,
            session_storage,
            system_prompt: SYSTEM_PROMPT.to_string(),
            compactor: None,
        }
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

    /// Override the wake-word detector.  Defaults to `InstantActivation` (no wait).
    pub fn with_wake_word_detector(mut self, detector: Arc<dyn WakeWordDetector>) -> Self {
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

        // Check if this is the first exchange (exactly 2 messages: user + assistant)
        if let Ok(msgs) = self.session_storage.get_messages(&self.session_id).await {
            if msgs.len() != 2 {
                return;
            }
        }

        // Build context for the title generation LLM call
        let context = format!(
            "User: {}\nAssistant: {}",
            user_text, assistant_text
        );
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
                        tracing::warn!("Failed to save session title: {}", e);
                    } else {
                        tracing::debug!("Auto-generated session title: {}", title);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Title generation failed (non-fatal): {}", e);
            }
        }
    }

    /// Streaming chat — routes through the Agent, chunks TTS by sentence.
    ///
    /// Differences from `chat_once`:
    /// - Calls `agent.chat_stream()` so text arrives token-by-token.
    /// - Speaks each completed sentence immediately (low-latency TTS).
    /// - Announces MCP tool calls with a short spoken phrase before execution.
    /// - Speaking happens *inside* this method; callers must NOT call
    ///   `voice_output.speak()` on the returned text.
    pub async fn chat_stream_once(&self, message: String) -> Result<String> {
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

        let request = AgentRequest {
            message: message.clone(),
            session_id: self.session_id.clone(),
            model_role: resolve_voice_role(&message),
        };

        let mut stream = self.agent.chat_stream(request).await?;
        let mut full_text = String::new();
        let mut sentence_buf = String::new();
        let mut spoken_first = false;
        let mut in_think_block = false;

        while let Some(event_result) = stream.next().await {
            match event_result? {
                AgentStreamEvent::ToolCall { tool, .. } => {
                    // Flush any buffered text before announcing the tool
                    if !sentence_buf.trim().is_empty() {
                        let chunk = sentence_buf.trim().to_string();
                        sentence_buf.clear();
                        if let Err(e) = self.voice_output.speak(&chunk).await {
                            tracing::warn!("TTS failed: {}", e);
                        }
                    }
                    let announcement = tool_announcement(&tool);
                    if let Err(e) = self.voice_output.speak(&announcement).await {
                        tracing::warn!("Tool announcement TTS failed: {}", e);
                    }
                }
                AgentStreamEvent::Text { content } => {
                    // Strip <think>…</think> reasoning blocks — not meant for TTS or transcript.
                    let (visible, new_in_think) = filter_thinking(&content, in_think_block);
                    in_think_block = new_in_think;
                    let content = visible;
                    if content.is_empty() {
                        continue;
                    }

                    if !spoken_first {
                        self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Speak));
                        spoken_first = true;
                    }
                    full_text.push_str(&content);
                    sentence_buf.push_str(&content);

                    let (sentences, remainder) = split_sentences(&sentence_buf);
                    sentence_buf = remainder;
                    for sentence in sentences {
                        let spoken = strip_markdown_for_speech(&sentence);
                        if spoken.is_empty() {
                            continue;
                        }
                        if let Err(e) = self.voice_output.speak(&spoken).await {
                            tracing::warn!("TTS failed: {}", e);
                        }
                    }
                }
                AgentStreamEvent::Done { .. } => {
                    // Flush any remaining buffer
                    let remainder = sentence_buf.trim().to_string();
                    if !remainder.is_empty() {
                        let spoken = strip_markdown_for_speech(&remainder);
                        if !spoken.is_empty() {
                            if let Err(e) = self.voice_output.speak(&spoken).await {
                                tracing::warn!("TTS flush failed: {}", e);
                            }
                        }
                    }
                    sentence_buf.clear();
                    break;
                }
                AgentStreamEvent::Error { content } => {
                    return Err(anyhow::anyhow!("Agent stream error: {}", content));
                }
                AgentStreamEvent::Status { .. } | AgentStreamEvent::ToolResult { .. } => {
                    // Not spoken — status/tool results are informational only
                }
            }
        }

        // Flush anything left if stream ended without Done
        let remainder = sentence_buf.trim().to_string();
        if !remainder.is_empty() {
            let spoken = strip_markdown_for_speech(&remainder);
            if !spoken.is_empty() {
                if let Err(e) = self.voice_output.speak(&spoken).await {
                    tracing::warn!("TTS final flush failed: {}", e);
                }
            }
        }

        // Persist the assistant response
        let assistant_msg = ChatMessage::assistant(full_text.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            assistant_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        self.maybe_generate_title(&message, &full_text).await;
        Ok(full_text)
    }

    /// Run the interactive workflow loop.
    ///
    /// State machine:
    ///   Wait → Listen → Thinking → Speak → (back to Wait)
    ///
    /// Input is obtained via the `VoiceInput` port (stdin by default).
    pub async fn run_loop(&self) -> Result<()> {
        loop {
            // ── Wait ──
            self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Wait));
            println!(
                "\n  🟢 {} (type \"exit\" to quit)",
                self.wake_word_detector.activation_prompt()
            );
            io::stdout().flush()?;
            self.wake_word_detector.wait_for_activation().await?;

            // ── Listen ──
            self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Listen));
            print!("  {}", self.voice_input.prompt());
            io::stdout().flush()?;

            let input = match self.voice_input.listen().await? {
                None => {
                    self.emit_event(WorkflowEvent::Exit);
                    println!("\n  ⏹ End of input.");
                    break;
                }
                Some(text) if text.is_empty() => continue,
                Some(text) => text,
            };

            if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                self.emit_event(WorkflowEvent::Exit);
                println!("  👋 Goodbye!");
                break;
            }

            self.emit_event(WorkflowEvent::UserInput(input.clone()));

            // ── Thinking → Speak (streaming) ──
            self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Thinking));
            println!("  🤔 Thinking...");

            match self.chat_stream_once(input).await {
                Ok(response_text) => {
                    // Speaking happened inside chat_stream_once; just emit the event.
                    self.emit_event(WorkflowEvent::AgentOutput(response_text));
                }
                Err(e) => {
                    eprintln!("  ❌ Error: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Hook point for future event subscribers (logging, UI, etc.).
    fn emit_event(&self, event: WorkflowEvent) {
        match &event {
            WorkflowEvent::StateChanged(state) => {
                tracing::debug!("Workflow state: {}", state);
            }
            WorkflowEvent::UserInput(text) => {
                tracing::debug!("User input: {}", text);
            }
            WorkflowEvent::AgentOutput(text) => {
                tracing::debug!("Agent output: {}", text);
            }
            WorkflowEvent::Exit => {
                tracing::debug!("Workflow exit requested");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mock_agent::MockAgent;
    use crate::services::mock_provider::MockProvider;
    use crate::services::mock_session::InMemorySessionStorage;

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

    #[tokio::test]
    async fn chat_persists_messages_to_storage() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service.chat_once("First message".to_string()).await.unwrap();

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

        let service = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_provider(provider);

        // First message → triggers title generation
        service.chat_once("What is the weather?".to_string()).await.unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        // MockProvider returns "Mock response to: ..." which becomes the title
        assert!(session.title.is_some(), "Title should be auto-generated after first exchange");
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

        let service = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_provider(provider);

        // First message → generates title
        service.chat_once("Hello".to_string()).await.unwrap();
        let first_title = storage.get_session(&session_id).await.unwrap().title.clone();

        // Second message → should NOT overwrite title
        service.chat_once("How are you?".to_string()).await.unwrap();
        let second_title = storage.get_session(&session_id).await.unwrap().title.clone();

        assert_eq!(first_title, second_title, "Title should not change after first generation");
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
        assert!(session.title.is_none(), "No title should be set without a provider");
    }

    #[tokio::test]
    async fn with_voice_input_builder_compiles() {
        use crate::services::stdin_input::StdinInput;
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let _service = ChatService::new(agent, session_id, storage)
            .with_voice_input(Arc::new(StdinInput::new()));
    }

    #[test]
    fn markdown_bold_stripped() {
        assert_eq!(strip_markdown_for_speech("The **quick** fox"), "The quick fox");
    }

    #[test]
    fn markdown_italic_stripped() {
        assert_eq!(strip_markdown_for_speech("The _quick_ fox"), "The quick fox");
        assert_eq!(strip_markdown_for_speech("The *quick* fox"), "The quick fox");
    }

    #[test]
    fn markdown_header_stripped() {
        assert_eq!(strip_markdown_for_speech("## Hello"), "Hello");
        assert_eq!(strip_markdown_for_speech("# Title\nBody text"), "Title Body text");
    }

    #[test]
    fn markdown_inline_code_stripped() {
        assert_eq!(strip_markdown_for_speech("Run `cargo build`"), "Run cargo build");
    }

    #[test]
    fn markdown_code_block_skipped() {
        let md = "Here is code:\n```\nfn main() {}\n```\nDone.";
        assert_eq!(strip_markdown_for_speech(md), "Here is code: Done.");
    }

    #[test]
    fn markdown_link_keeps_text() {
        assert_eq!(
            strip_markdown_for_speech("See [the docs](https://example.com)"),
            "See the docs"
        );
    }

    #[test]
    fn markdown_image_dropped() {
        assert_eq!(strip_markdown_for_speech("![logo](logo.png) text"), "text");
    }

    #[test]
    fn markdown_list_items_stripped() {
        let md = "- item one\n- item two";
        assert_eq!(strip_markdown_for_speech(md), "item one item two");
    }

    #[test]
    fn markdown_hr_dropped() {
        assert_eq!(strip_markdown_for_speech("before\n---\nafter"), "before after");
    }

    #[test]
    fn markdown_blockquote_stripped() {
        assert_eq!(strip_markdown_for_speech("> quoted text"), "quoted text");
    }

    #[test]
    fn filter_thinking_strips_complete_block() {
        let (text, in_block) = filter_thinking("<think>reasoning here</think>actual answer", false);
        assert_eq!(text, "actual answer");
        assert!(!in_block);
    }

    #[test]
    fn filter_thinking_no_block_passthrough() {
        let (text, in_block) = filter_thinking("just normal text", false);
        assert_eq!(text, "just normal text");
        assert!(!in_block);
    }

    #[test]
    fn filter_thinking_split_across_chunks() {
        // First chunk opens the block but doesn't close it
        let (text1, in_block) = filter_thinking("prefix<think>start of reasoning", false);
        assert_eq!(text1, "prefix");
        assert!(in_block);

        // Second chunk closes it and continues with real content
        let (text2, in_block2) = filter_thinking("end of reasoning</think>real answer", in_block);
        assert_eq!(text2, "real answer");
        assert!(!in_block2);
    }

    #[test]
    fn filter_thinking_chunk_entirely_inside_block() {
        let (text, in_block) = filter_thinking("more reasoning tokens", true);
        assert_eq!(text, "");
        assert!(in_block);
    }

    #[test]
    fn filter_thinking_multiple_blocks() {
        let (text, in_block) = filter_thinking(
            "<think>a</think>first<think>b</think>second", false
        );
        assert_eq!(text, "firstsecond");
        assert!(!in_block);
    }

    #[tokio::test]
    async fn with_voice_output_builder_compiles() {
        use crate::services::print_output::PrintOutput;
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let _service = ChatService::new(agent, session_id, storage)
            .with_voice_output(Arc::new(PrintOutput));
    }

}
