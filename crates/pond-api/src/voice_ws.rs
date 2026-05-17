//! WebSocket handler for real-time voice sessions.
//!
//! Protocol:
//!   1. Client opens `GET /api/v1/voice/session` → WebSocket upgrade
//!   2. Client sends JSON `{"type":"start","session_id":"..."}` (optional session_id)
//!   3. Client sends Binary frame (WAV audio bytes)
//!   4. Server transcribes via Whisper, streams agent response with sentence-level
//!      TTS, and sends a done frame
//!   5. Client may send another Binary frame (next turn) or close the socket
//!
//! Only one voice session is allowed at a time (guarded by `voice_session_lock`).

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use pond_core::domain::agent::{AgentRequest, AgentStreamEvent};
use pond_core::domain::message::ChatMessage;
use pond_core::domain::session::SessionMessage;
use pond_core::services::chat::{split_sentences, strip_markdown_for_speech};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use crate::thought_filter::ThoughtFilter;
use crate::AppState;

/// JSON messages the client can send over the WebSocket.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ClientMsg {
    /// Begin a voice session, optionally resuming an existing conversation.
    #[serde(rename = "start")]
    Start { session_id: Option<String> },
    /// Interrupt the current agent response (stop TTS, drop the stream).
    #[serde(rename = "interrupt")]
    Interrupt,
}

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum size of a single WAV upload (30 seconds of 16-bit mono 16 kHz).
const MAX_AUDIO_BYTES: usize = 960_000;

/// How long to wait for the initial Start message before giving up.
const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long to wait for audio data after Start before timing out.
const AUDIO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

// ── Handler ──────────────────────────────────────────────────────────────────

/// Axum handler: upgrade the HTTP request to a WebSocket.
pub async fn voice_session_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.max_message_size(MAX_AUDIO_BYTES + 4096)
        .on_upgrade(move |socket| handle_voice_session(socket, state))
}

/// Main voice session loop. Runs for the lifetime of the WebSocket.
async fn handle_voice_session(mut socket: WebSocket, state: Arc<AppState>) {
    // ── 1. Acquire exclusive voice session lock ──────────────────────────────
    let guard = match state.voice_session_lock.try_lock() {
        Ok(g) => g,
        Err(_) => {
            let _ = send_json(
                &mut socket,
                &json!({"type": "error", "message": "Voice session already active"}),
            )
            .await;
            let _ = socket.close().await;
            return;
        }
    };

    // ── 2. Wait for Start message ────────────────────────────────────────────
    let session_id = match wait_for_start(&mut socket).await {
        Some(id) => id,
        None => {
            drop(guard);
            return;
        }
    };

    // Ensure session exists in storage
    let storage = &state.session_storage;
    if storage.get_session(&session_id).await.is_err() {
        if let Err(e) = storage.create_session(session_id.clone()).await {
            let _ = send_json(
                &mut socket,
                &json!({"type": "error", "message": format!("Failed to create session: {e}")}),
            )
            .await;
            drop(guard);
            return;
        }
    }

    let _ = send_json(
        &mut socket,
        &json!({"type": "ready", "session_id": &session_id}),
    )
    .await;

    // ── 3. Turn loop: receive audio, transcribe, respond ─────────────────────
    loop {
        let wav_bytes = match wait_for_audio(&mut socket).await {
            AudioResult::Audio(bytes) => bytes,
            AudioResult::Closed => break,
            AudioResult::Interrupt => continue,
            AudioResult::Error(msg) => {
                let _ = send_json(&mut socket, &json!({"type": "error", "message": msg})).await;
                continue;
            }
        };

        // ── 4. Transcribe via Whisper ────────────────────────────────────────
        let transcript = match transcribe_audio(&state, &wav_bytes).await {
            Ok(text) => text,
            Err(e) => {
                let _ = send_json(
                    &mut socket,
                    &json!({"type": "error", "message": format!("Transcription failed: {e}")}),
                )
                .await;
                continue;
            }
        };

        if transcript.trim().is_empty() {
            let _ = send_json(&mut socket, &json!({"type": "transcript", "text": ""})).await;
            continue;
        }

        let _ = send_json(
            &mut socket,
            &json!({"type": "transcript", "text": &transcript}),
        )
        .await;

        // ── 5. Persist user message ──────────────────────────────────────────
        {
            let user_msg = ChatMessage::user(transcript.clone());
            let sm = SessionMessage::new(Uuid::new_v4().to_string(), session_id.clone(), user_msg);
            if let Err(e) = storage.add_message(session_id.clone(), sm).await {
                tracing::warn!(session_id = %session_id, "Failed to persist user message: {e}");
            }
        }

        // ── 6. Stream agent response with sentence-level TTS ─────────────────
        let agent_req = AgentRequest {
            message: transcript.clone(),
            session_id: session_id.clone(),
            model_role: "chat".to_string(),
            images: Vec::new(),
            voice_mode: true,
            canvas_mode: false,
        };

        let agent_stream = match state.agent.chat_stream(agent_req).await {
            Ok(s) => s,
            Err(e) => {
                let _ = send_json(
                    &mut socket,
                    &json!({"type": "error", "message": format!("Agent error: {e}")}),
                )
                .await;
                continue;
            }
        };

        let full_text = stream_response_with_tts(&mut socket, &state, agent_stream).await;

        // ── 7. Persist assistant message ─────────────────────────────────────
        if !full_text.is_empty() {
            let assistant_msg = ChatMessage::assistant(full_text.clone());
            let sm = SessionMessage::new(
                Uuid::new_v4().to_string(),
                session_id.clone(),
                assistant_msg,
            );
            if let Err(e) = storage.add_message(session_id.clone(), sm).await {
                tracing::warn!(session_id = %session_id, "Failed to persist assistant message: {e}");
            }
        }

        // ── 8. Done ──────────────────────────────────────────────────────────
        let _ = send_json(
            &mut socket,
            &json!({"type": "done", "session_id": &session_id}),
        )
        .await;
    }

    drop(guard);
    tracing::debug!("Voice WebSocket session ended");
}

// ── Transcription ────────────────────────────────────────────────────────────

/// POST the WAV bytes to the Whisper server and return the transcript text.
async fn transcribe_audio(state: &AppState, wav_bytes: &[u8]) -> anyhow::Result<String> {
    let whisper_url = format!("{}/inference", state.whisper_url);
    let part = reqwest::multipart::Part::bytes(wav_bytes.to_vec())
        .file_name("audio.wav")
        .mime_str("audio/wav")?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json");

    let resp = state
        .http_client
        .post(&whisper_url)
        .multipart(form)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Whisper returned {status}: {body}");
    }

    let json: serde_json::Value = resp.json().await?;
    Ok(json["text"].as_str().unwrap_or("").trim().to_string())
}

// ── TTS synthesis ────────────────────────────────────────────────────────────

/// Synthesize text to WAV via the Piper HTTP server. Returns `None` if Piper
/// is not running or synthesis fails (non-fatal — the text is still sent).
async fn synthesize_tts(state: &AppState, text: &str) -> Option<Vec<u8>> {
    let port = state.piper_http_port?;
    let piper_url = format!("http://127.0.0.1:{port}/tts");

    let resp = state
        .http_client
        .post(&piper_url)
        .header("content-type", "text/plain; charset=utf-8")
        .body(text.to_string())
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        tracing::debug!("Piper TTS returned {}", resp.status());
        return None;
    }

    resp.bytes().await.ok().map(|b| b.to_vec())
}

// ── Agent streaming with TTS ─────────────────────────────────────────────────

/// Consume the agent stream, apply ThoughtFilter, split into sentences,
/// synthesize TTS for each, and send everything over the WebSocket.
///
/// Returns the full accumulated response text (for persistence).
async fn stream_response_with_tts(
    socket: &mut WebSocket,
    state: &AppState,
    agent_stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<AgentStreamEvent, anyhow::Error>> + Send>,
    >,
) -> String {
    let mut thought = ThoughtFilter::new();
    let mut full_text = String::new();
    let mut sentence_buf = String::new();

    tokio::pin!(agent_stream);

    while let Some(event_result) = agent_stream.next().await {
        match event_result {
            Ok(AgentStreamEvent::Text { content }) => {
                let visible = thought.push(&content);
                if visible.is_empty() {
                    continue;
                }
                full_text.push_str(&visible);
                sentence_buf.push_str(&visible);

                // Send partial text token to client
                let _ = send_json(socket, &json!({"type": "text", "token": &visible})).await;

                // Split completed sentences and synthesize TTS
                let (sentences, remainder) = split_sentences(&sentence_buf);
                sentence_buf = remainder;
                for sentence in sentences {
                    let spoken = strip_markdown_for_speech(&sentence);
                    if spoken.is_empty() {
                        continue;
                    }
                    if let Some(wav) = synthesize_tts(state, &spoken).await {
                        let _ = socket.send(Message::Binary(wav.into())).await;
                    }
                }
            }
            Ok(AgentStreamEvent::ToolCall { tool, id, input }) => {
                let _ = send_json(
                    socket,
                    &json!({"type": "tool_call", "tool": &tool, "id": &id, "input": input}),
                )
                .await;
            }
            Ok(AgentStreamEvent::ToolResult { tool, id, content }) => {
                let _ = send_json(
                    socket,
                    &json!({"type": "tool_result", "tool": &tool, "id": &id, "content": &content}),
                )
                .await;
            }
            Ok(AgentStreamEvent::Done { .. }) => break,
            Ok(AgentStreamEvent::Error { content }) => {
                let _ = send_json(socket, &json!({"type": "error", "message": &content})).await;
                break;
            }
            Ok(AgentStreamEvent::Status { content }) => {
                let _ = send_json(socket, &json!({"type": "status", "content": &content})).await;
            }
            Ok(
                AgentStreamEvent::Thinking { .. }
                | AgentStreamEvent::ReviewStatus { .. }
                | AgentStreamEvent::ReviewRevision { .. },
            ) => {
                // Not surfaced in voice sessions
            }
            Err(e) => {
                let _ = send_json(
                    socket,
                    &json!({"type": "error", "message": format!("Stream error: {e}")}),
                )
                .await;
                break;
            }
        }
    }

    // Flush the thought filter tail
    let tail = thought.flush();
    if !tail.is_empty() {
        full_text.push_str(&tail);
        sentence_buf.push_str(&tail);
    }

    // Speak any remaining buffered text
    let remainder = sentence_buf.trim().to_string();
    if !remainder.is_empty() {
        let spoken = strip_markdown_for_speech(&remainder);
        if !spoken.is_empty() {
            if let Some(wav) = synthesize_tts(state, &spoken).await {
                let _ = socket.send(Message::Binary(wav.into())).await;
            }
        }
    }

    full_text
}

// ── WebSocket message helpers ────────────────────────────────────────────────

/// Result of waiting for audio from the client.
enum AudioResult {
    Audio(Vec<u8>),
    Closed,
    Interrupt,
    Error(String),
}

/// Wait for the Start message. Returns the session ID on success.
async fn wait_for_start(socket: &mut WebSocket) -> Option<String> {
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        let msg = tokio::select! {
            msg = socket.recv() => msg,
            _ = tokio::time::sleep_until(deadline) => {
                let _ = send_json(
                    socket,
                    &json!({"type": "error", "message": "Timed out waiting for start message"}),
                ).await;
                return None;
            }
        };

        match msg {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(&text) {
                Ok(ClientMsg::Start { session_id }) => {
                    return Some(session_id.unwrap_or_else(|| Uuid::new_v4().to_string()));
                }
                Ok(ClientMsg::Interrupt) => continue,
                Err(e) => {
                    let _ = send_json(
                        socket,
                        &json!({"type": "error", "message": format!("Invalid message: {e}")}),
                    )
                    .await;
                    continue;
                }
            },
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Ok(Message::Ping(data))) => {
                let _ = socket.send(Message::Pong(data)).await;
            }
            _ => continue,
        }
    }
}

/// Wait for a Binary audio frame from the client.
async fn wait_for_audio(socket: &mut WebSocket) -> AudioResult {
    let deadline = tokio::time::Instant::now() + AUDIO_TIMEOUT;
    loop {
        let msg = tokio::select! {
            msg = socket.recv() => msg,
            _ = tokio::time::sleep_until(deadline) => {
                return AudioResult::Error("Timed out waiting for audio".to_string());
            }
        };

        match msg {
            Some(Ok(Message::Binary(data))) => {
                let bytes = data.to_vec();
                if bytes.len() > MAX_AUDIO_BYTES {
                    return AudioResult::Error(format!(
                        "Audio too large: {} bytes (max {})",
                        bytes.len(),
                        MAX_AUDIO_BYTES,
                    ));
                }
                return AudioResult::Audio(bytes);
            }
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(ClientMsg::Interrupt) => return AudioResult::Interrupt,
                    Ok(ClientMsg::Start { .. }) => {
                        // Duplicate start — ignore
                        continue;
                    }
                    Err(_) => continue,
                }
            }
            Some(Ok(Message::Close(_))) | None => return AudioResult::Closed,
            Some(Ok(Message::Ping(data))) => {
                if let Err(_) = socket.send(Message::Pong(data)).await {
                    return AudioResult::Closed;
                }
            }
            _ => continue,
        }
    }
}

/// Send a JSON message as a Text frame.
async fn send_json(socket: &mut WebSocket, value: &serde_json::Value) -> Result<(), axum::Error> {
    socket.send(Message::Text(value.to_string().into())).await
}
