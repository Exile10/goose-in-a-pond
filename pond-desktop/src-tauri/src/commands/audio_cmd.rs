use crate::audio::{self, AudioState, WakeListenerState};
use crate::canvas_feed::dispatch_sse_event;
use crate::process::ServerProcess;
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptResult {
    pub text: String,
}

/// Quips spoken by Goose while it processes your request.
/// Short phrases — aim for ≤ 2 seconds of synthesised audio each.
const QUIPS: &[&str] = &[
    "On it.",
    "Let me think.",
    "Ruffling through possibilities.",
    "Consulting the pond elders.",
    "Wading into the knowledge pool.",
    "Hatching a response.",
    "Migrating toward an answer.",
    "Paddling upstream.",
    "Preening my thoughts.",
    "Assembling ideas, feather by feather.",
    "Surveying the flock.",
    "Squinting at the data.",
    "Flocking toward clarity.",
    "Skimming the surface.",
];

/// Pick a quip using sub-millisecond time as a cheap source of variety.
fn pick_quip() -> &'static str {
    let idx = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
        % QUIPS.len();
    QUIPS[idx]
}

/// Begin capturing audio from the microphone.
/// Emits `audio-level` events (f32 0..1) for the VoiceOrb animation.
#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    audio_state: State<'_, AudioState>,
) -> Result<(), String> {
    let app_clone = app.clone();
    audio::start_capture(&audio_state, move |level| {
        let _ = app_clone.emit("audio-level", level);
    })?;
    let _ = app.emit("recording-started", ());
    Ok(())
}

/// Stop recording. Waits for the capture thread to drain, then returns WAV bytes.
#[tauri::command]
pub async fn stop_recording(
    audio_state: State<'_, AudioState>,
) -> Result<Vec<u8>, String> {
    // Run blocking stop on a thread pool so we don't block the Tauri async runtime
    let samples = audio_state.samples.clone();
    let is_recording = audio_state.is_recording.clone();
    let native_rate = audio_state.native_sample_rate.clone();
    let stop_tx = audio_state.stop_tx.clone();

    tokio::task::spawn_blocking(move || {
        // Signal stop
        if let Some(tx) = stop_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        // Wait for thread to finish (max 2s)
        for _ in 0..40 {
            if !is_recording.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let captured = samples.lock().unwrap().clone();
        if captured.is_empty() {
            return Err("No audio captured".to_string());
        }
        let rate = *native_rate.lock().unwrap();
        // Normalise to 16 kHz — same as the wake listener — so the
        // transcribe endpoint always receives a consistent sample rate.
        let (pcm, wav_rate) = if rate != 16000 {
            (audio::resample_linear(&captured, rate, 16000), 16000u32)
        } else {
            (captured, rate)
        };
        audio::encode_wav(&pcm, wav_rate)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Abort recording without returning audio.
#[tauri::command]
pub async fn abort_recording(
    audio_state: State<'_, AudioState>,
    app: AppHandle,
) -> Result<(), String> {
    audio::abort_capture(&audio_state);
    let _ = app.emit("recording-aborted", ());
    Ok(())
}

/// Start the passive wake-word listening loop.
///
/// Captures audio in 0.8s chunks, applies a silence gate, transcribes non-silent
/// chunks via the pond-server /api/v1/transcribe endpoint, and emits
/// `wake-word-detected` when `wake_word` is found in the transcript.
#[tauri::command]
pub async fn start_wake_listener(
    app: AppHandle,
    wake_word: String,
    wake_state: State<'_, WakeListenerState>,
    server: State<'_, ServerProcess>,
) -> Result<(), String> {
    let base_url = server.get_url();
    audio::start_wake_listener(&wake_state, wake_word, base_url, app)
}

/// Stop the passive wake-word listening loop.
#[tauri::command]
pub async fn stop_wake_listener(
    wake_state: State<'_, WakeListenerState>,
) -> Result<(), String> {
    audio::stop_wake_listener(&wake_state);
    Ok(())
}

/// Full voice pipeline:
/// (caller passes WAV bytes from stop_recording) → transcribe → chat stream → speak
///
/// A short quip is synthesised in the background immediately so the silence
/// between the user speaking and Goose responding is filled with audio.
///
/// `auth_token` — the pond session token from localStorage; empty string for unauthenticated
///   loopback connections (pond-server accepts any well-formed Bearer on loopback).
///
/// Emits:
///   `transcript`      — {text: string}
///   `response-token`  — {token: string, done: bool}
///   `tool-result`     — {tool: string, data: object, timestamp_ms: number}
///   `tts-start`
///   `tts-end`
///   `pipeline-error`  — string
#[tauri::command]
pub async fn run_voice_pipeline(
    app: AppHandle,
    wav_bytes: Vec<u8>,
    auth_token: String,
    session_id: Option<String>,
    server: State<'_, ServerProcess>,
) -> Result<(), String> {
    let base_url = server.get_url();
    let client = reqwest::Client::new();

    // Build a helper that adds the Authorization header when a token is present.
    let bearer = if auth_token.is_empty() {
        None
    } else {
        Some(format!("Bearer {}", auth_token))
    };

    // ── 0. Start quip synthesis + playback immediately ──────────────────────
    // The quip is fetched AND played inside a spawned task so it runs
    // concurrently with transcription + LLM inference.  The user hears audio
    // during the silence between speaking and the model's first token — not
    // after the model finishes.
    let quip_text   = pick_quip();
    let quip_client = client.clone();
    let quip_url    = base_url.clone();
    let quip_handle = tokio::spawn(async move {
        match fetch_tts_bytes(&quip_client, &quip_url, quip_text).await {
            Ok(bytes) => { let _ = play_wav_bytes(bytes).await; }
            Err(e)    => { tracing::debug!("Quip TTS skipped: {e}"); }
        }
    });

    // ── 1. Transcribe ────────────────────────────────────────────────────────
    let part = multipart::Part::bytes(wav_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = multipart::Form::new().part("audio", part);

    let mut transcribe_req = client
        .post(format!("{}/api/v1/transcribe", base_url))
        .multipart(form);
    if let Some(ref auth) = bearer {
        transcribe_req = transcribe_req.header("Authorization", auth.as_str());
    }

    let transcript_res = transcribe_req
        .send()
        .await
        .map_err(|e| {
            let msg = format!("Transcribe request failed: {e}");
            let _ = app.emit("pipeline-error", &msg);
            msg
        })?;

    if !transcript_res.status().is_success() {
        let msg = format!("Transcribe error: {}", transcript_res.status());
        let _ = app.emit("pipeline-error", &msg);
        return Err(msg);
    }

    let TranscriptResult { text: transcript } = transcript_res
        .json::<TranscriptResult>()
        .await
        .map_err(|e| format!("Failed to parse transcript: {e}"))?;

    let _ = app.emit("transcript", TranscriptResult { text: transcript.clone() });

    // ── 2. Chat (streaming SSE) — runs while quip is already playing ─────────
    let effective_session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let chat_req = serde_json::json!({
        "message": transcript,
        "session_id": effective_session_id
    });

    let mut chat_builder = client
        .post(format!("{}/api/v1/chat/stream", base_url))
        .json(&chat_req);
    if let Some(ref auth) = bearer {
        chat_builder = chat_builder.header("Authorization", auth.as_str());
    }

    let mut chat_res = chat_builder
        .send()
        .await
        .map_err(|e| {
            let msg = format!("Chat request failed: {e}");
            let _ = app.emit("pipeline-error", &msg);
            msg
        })?;

    let mut response_text = String::new();
    while let Some(chunk) = chat_res
        .chunk()
        .await
        .map_err(|e| format!("Stream error: {e}"))?
    {
        let text = String::from_utf8_lossy(&chunk);
        for line in text.lines() {
            dispatch_sse_event(&app, line);
            // Collect plain text for TTS.
            // Supports both canonical {"type":"text","content":"..."} and
            // legacy {"token":"..."} formats for forward/backward compat.
            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                    if val.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(c) = val.get("content").and_then(|c| c.as_str()) {
                            response_text.push_str(c);
                        }
                    } else if let Some(tok) = val.get("token").and_then(|t| t.as_str()) {
                        response_text.push_str(tok);
                    }
                }
            }
        }
    }

    // ── 3. TTS playback: wait for quip → then play response ──────────────────
    // quip_handle may still be playing (or already finished).  Awaiting it
    // ensures we don't cut the quip short before starting the response audio.
    let _ = app.emit("tts-start", ());
    quip_handle.await.ok();

    if !response_text.is_empty() {
        if let Err(e) = play_tts(&client, &base_url, &response_text).await {
            tracing::warn!("TTS playback failed (non-fatal): {e}");
        }
    }

    let _ = app.emit("tts-end", ());

    Ok(())
}

/// Fetch synthesised WAV bytes for `text` from the pond-server TTS endpoint.
/// Does NOT play audio — just returns the bytes.
async fn fetch_tts_bytes(
    client: &reqwest::Client,
    base_url: &str,
    text: &str,
) -> Result<Vec<u8>, String> {
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client
            .post(format!("{}/api/v1/tts", base_url))
            .json(&serde_json::json!({ "text": text }))
            .send(),
    )
    .await
    .map_err(|_| "TTS fetch timed out".to_string())?
    .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!("TTS server error: {}", res.status()));
    }

    res.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
}

/// Play WAV bytes through the system audio output via rodio.
async fn play_wav_bytes(bytes: Vec<u8>) -> Result<(), String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            use rodio::{Decoder, OutputStream, Sink};
            let (_stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
            let sink = Sink::try_new(&handle).map_err(|e| e.to_string())?;
            let cursor = std::io::Cursor::new(bytes);
            let source = Decoder::new(cursor).map_err(|e| e.to_string())?;
            sink.append(source);
            sink.sleep_until_end();
            Ok::<_, String>(())
        }),
    )
    .await
    .map_err(|_| "Audio playback timed out".to_string())?
    .map_err(|e| e.to_string())?
}

/// Synthesise `text` and play it — convenience wrapper for the main response.
async fn play_tts(client: &reqwest::Client, base_url: &str, text: &str) -> Result<(), String> {
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        fetch_tts_bytes(client, base_url, text),
    )
    .await
    .map_err(|_| "TTS HTTP request timed out after 30s".to_string())??;

    play_wav_bytes(bytes).await
}
