use crate::audio::{self, AudioState};
use crate::canvas_feed::dispatch_sse_event;
use crate::process::ServerProcess;
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptResult {
    pub text: String,
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
        audio::encode_wav(&captured, rate)
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

/// Full voice pipeline:
/// (caller passes WAV bytes from stop_recording) → transcribe → chat stream → speak
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
    server: State<'_, ServerProcess>,
) -> Result<(), String> {
    let base_url = server.get_url();
    let client = reqwest::Client::new();

    // ── 1. Transcribe ────────────────────────────────────────────────────────
    let part = multipart::Part::bytes(wav_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = multipart::Form::new().part("file", part);

    let transcript_res = client
        .post(format!("{}/api/v1/transcribe", base_url))
        .multipart(form)
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

    // ── 2. Chat (streaming SSE) ──────────────────────────────────────────────
    let chat_req = serde_json::json!({
        "message": transcript,
        "session_id": "canvas-voice"
    });

    // Note: pond-server exempts loopback from auth — empty token is fine here
    let mut chat_res = client
        .post(format!("{}/api/v1/chat/stream", base_url))
        .json(&chat_req)
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
            // Collect plain text for TTS
            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                    if val.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(c) = val.get("content").and_then(|c| c.as_str()) {
                            response_text.push_str(c);
                        }
                    }
                }
            }
        }
    }

    // ── 3. TTS playback ──────────────────────────────────────────────────────
    if !response_text.is_empty() {
        let _ = app.emit("tts-start", ());
        let tts_result = play_tts(&client, &base_url, &response_text).await;
        let _ = app.emit("tts-end", ());
        if let Err(e) = tts_result {
            tracing::warn!("TTS playback failed (non-fatal): {e}");
        }
    }

    Ok(())
}

async fn play_tts(client: &reqwest::Client, base_url: &str, text: &str) -> Result<(), String> {
    let res = client
        .post(format!("{}/api/v1/tts", base_url))
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!("TTS server error: {}", res.status()));
    }

    let bytes = res.bytes().await.map_err(|e| e.to_string())?.to_vec();

    tokio::task::spawn_blocking(move || {
        use rodio::{Decoder, OutputStream, Sink};
        let (_stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
        let sink = Sink::try_new(&handle).map_err(|e| e.to_string())?;
        let cursor = std::io::Cursor::new(bytes);
        let source = Decoder::new(cursor).map_err(|e| e.to_string())?;
        sink.append(source);
        sink.sleep_until_end();
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())?
}
