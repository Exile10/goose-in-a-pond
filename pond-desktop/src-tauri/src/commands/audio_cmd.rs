use crate::audio::{self, AudioState, WakeListenerState};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

// ── Q2-26 speculative LLM slot ─────────────────────────────────────────────
// Holds a live HTTP response from a speculative POST to /api/v1/chat/stream
// that was fired as soon as speculative ASR resolved (mid-silence window).
// `run_voice_pipeline` consumes it so the LLM has already been running for
// (silence_window − asr_latency) ms before the pipeline even starts.

/// Global audio kill switch — stops ALL Goose audio (TTS + thinking tone)
/// when the wake word is detected. Checked by `play_wav_interruptible` every
/// 50 ms during playback. Reset at the start of each voice pipeline run.
pub struct AudioKillSwitch(pub Arc<AtomicBool>);

impl AudioKillSwitch {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

/// Tracks whether the voice pipeline is currently active (transcribe → chat →
/// TTS).  The always-on wake listener reads this to decide between:
///   - `false` → initial activation: full one-breath capture + `wake-word-detected`
///   - `true`  → barge-in: just set kill switch + `wake-word-interrupt`
pub struct PipelineActive(pub Arc<AtomicBool>);

impl PipelineActive {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

/// `rodio::OutputStream` is `!Send` due to cpal's CoreAudio property-listener
/// callbacks. We move it to a dedicated keeper thread that lives for the whole
/// app lifetime and never touch it from any other thread, so the transfer is
/// safe. Mirrors `pond_adapters_piper::AudioKeeper`.
#[allow(dead_code)] // kept alive for its Drop (closes the audio device); never read
struct SendableStream(rodio::OutputStream);
// SAFETY: moved into the keeper thread exactly once at app startup and never
// accessed from any other thread afterward.
unsafe impl Send for SendableStream {}

/// Begin capturing audio from the microphone.
/// Emits `audio-level` events (f32 0..1) for the VoiceOrb animation.
#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    audio_state: State<'_, AudioState>,
    voice: State<'_, crate::chat_process::VoiceChatProcess>,
) -> Result<(), String> {
    // A terminal-voice child owns the mic exclusively while active — refuse to
    // open a second capture stream that would fight it for the device.
    if voice.is_active() {
        return Err("voice session active".to_string());
    }

    let app_clone = app.clone();
    audio::start_capture(&audio_state, move |level| {
        let _ = app_clone.emit("audio-level", level);
    })?;
    let _ = app.emit("recording-started", ());
    Ok(())
}

/// Stop recording. Waits for the capture thread to drain, then returns WAV bytes.
#[tauri::command]
pub async fn stop_recording(audio_state: State<'_, AudioState>) -> Result<Vec<u8>, String> {
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

/// Stop the passive wake-word listening loop.
#[tauri::command]
pub async fn stop_wake_listener(wake_state: State<'_, WakeListenerState>) -> Result<(), String> {
    audio::stop_wake_listener(&wake_state);
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

    res.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

/// Play WAV bytes through the system audio output via rodio.
/// Play WAV bytes with an interruptible loop. Checks `kill` every 50 ms;
/// if set, stops playback immediately (wake word barge-in).
///
/// `shared_handle` should be the app-lifetime `SharedAudioOutput` handle —
/// reusing it avoids reopening the CoreAudio device on every call, which
/// fragments playback across a multi-sentence response. Falls back to
/// opening a fresh, one-off stream only if no shared handle is available.
async fn play_wav_bytes_interruptible(
    bytes: Vec<u8>,
    kill: Arc<AtomicBool>,
    shared_handle: Option<rodio::OutputStreamHandle>,
) -> Result<(), String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            use rodio::{Decoder, OutputStream, Sink};
            let (_owned_stream, handle) = match shared_handle {
                Some(h) => (None, h),
                None => {
                    let (stream, handle) =
                        OutputStream::try_default().map_err(|e| e.to_string())?;
                    (Some(stream), handle)
                }
            };
            let sink = Sink::try_new(&handle).map_err(|e| e.to_string())?;
            let cursor = std::io::Cursor::new(bytes);
            let source = Decoder::new(cursor).map_err(|e| e.to_string())?;
            sink.append(source);
            // Poll instead of sleep_until_end so the kill switch can stop us.
            while !sink.empty() {
                if kill.load(Ordering::Relaxed) {
                    sink.stop();
                    tracing::debug!("Audio playback killed by wake word");
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok::<_, String>(())
        }),
    )
    .await
    .map_err(|_| "Audio playback timed out".to_string())?
    .map_err(|e| e.to_string())?
}

/// Non-interruptible playback — used for short pings where barge-in is unwanted.
#[allow(dead_code)]
async fn play_wav_bytes(bytes: Vec<u8>) -> Result<(), String> {
    play_wav_bytes_interruptible(bytes, Arc::new(AtomicBool::new(false)), None).await
}

/// Synthesise `text` and play it — convenience wrapper.
/// No longer used by the sentence-streaming pipeline but kept for utility.
#[allow(dead_code)]
async fn play_tts(client: &reqwest::Client, base_url: &str, text: &str) -> Result<(), String> {
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        fetch_tts_bytes(client, base_url, text),
    )
    .await
    .map_err(|_| "TTS HTTP request timed out after 30s".to_string())??;

    play_wav_bytes(bytes).await
}
