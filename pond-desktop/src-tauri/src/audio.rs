/// Audio capture + wake-word listener module.
///
/// cpal::Stream is !Send so it cannot be stored in Tauri managed state directly.
/// Instead we keep only Arc/AtomicBool in managed state and run the cpal stream
/// on a dedicated OS thread that lives as long as recording is active.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tauri::Emitter;
use cpal::{SampleFormat, SampleRate, StreamConfig};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

/// Shared audio state — stored in Tauri's managed state map.
/// All fields are Send + Sync so Tauri is happy.
pub struct AudioState {
    /// Accumulated 16-bit mono 16 kHz PCM samples from the current recording.
    pub samples: Arc<Mutex<Vec<i16>>>,
    /// Set to `true` while a recording is in progress.
    pub is_recording: Arc<AtomicBool>,
    /// Sample rate reported by the hardware (needed for WAV encoding).
    pub native_sample_rate: Arc<Mutex<u32>>,
    /// Channel: send `()` to stop the recording thread gracefully.
    pub stop_tx: Arc<Mutex<Option<std::sync::mpsc::Sender<()>>>>,
}

impl AudioState {
    pub fn new() -> Self {
        Self {
            samples: Arc::new(Mutex::new(Vec::new())),
            is_recording: Arc::new(AtomicBool::new(false)),
            native_sample_rate: Arc::new(Mutex::new(16000)),
            stop_tx: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for AudioState {
    fn default() -> Self {
        Self::new()
    }
}

/// Managed state for the background wake-word listening loop.
pub struct WakeListenerState {
    pub is_running: Arc<AtomicBool>,
    pub stop_tx: Arc<Mutex<Option<std::sync::mpsc::Sender<()>>>>,
}

impl WakeListenerState {
    pub fn new() -> Self {
        Self {
            is_running: Arc::new(AtomicBool::new(false)),
            stop_tx: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for WakeListenerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Start audio capture on a background thread.
/// `on_level` receives RMS energy (0..1) for each audio frame — used to animate the VoiceOrb.
pub fn start_capture<F>(state: &AudioState, on_level: F) -> Result<(), String>
where
    F: Fn(f32) + Send + Sync + 'static,
{
    if state.is_recording.load(Ordering::SeqCst) {
        return Err("Already recording".to_string());
    }

    // Clear previous samples
    state.samples.lock().unwrap().clear();
    state.is_recording.store(true, Ordering::SeqCst);

    let samples_arc = Arc::clone(&state.samples);
    let is_recording_arc = Arc::clone(&state.is_recording);
    let native_rate_arc = Arc::clone(&state.native_sample_rate);

    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    *state.stop_tx.lock().unwrap() = Some(stop_tx);

    thread::spawn(move || {
        let result = capture_thread(samples_arc, is_recording_arc.clone(), native_rate_arc, on_level, stop_rx);
        is_recording_arc.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            tracing::error!("Audio capture thread error: {e}");
        }
    });

    Ok(())
}

/// Signal the capture thread to stop and wait for it to drain.
/// Returns the WAV-encoded audio bytes.
#[allow(dead_code)]
pub fn stop_capture(state: &AudioState) -> Result<Vec<u8>, String> {
    // Send stop signal
    if let Some(tx) = state.stop_tx.lock().unwrap().take() {
        let _ = tx.send(());
    }

    // Poll until the thread finishes (max 2s)
    for _ in 0..40 {
        if !state.is_recording.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    let samples = state.samples.lock().unwrap().clone();
    if samples.is_empty() {
        return Err("No audio captured".to_string());
    }

    let native_rate = *state.native_sample_rate.lock().unwrap();

    let resampled = if native_rate != 16000 {
        resample_linear(&samples, native_rate, 16000)
    } else {
        samples
    };

    encode_wav(&resampled, 16000)
}

/// Abort recording without returning audio.
pub fn abort_capture(state: &AudioState) {
    if let Some(tx) = state.stop_tx.lock().unwrap().take() {
        let _ = tx.send(());
    }
    state.is_recording.store(false, Ordering::SeqCst);
}

/// The OS thread function: opens cpal, streams PCM, writes to shared buffer.
fn capture_thread<F>(
    samples: Arc<Mutex<Vec<i16>>>,
    is_recording: Arc<AtomicBool>,
    native_rate: Arc<Mutex<u32>>,
    on_level: F,
    stop_rx: std::sync::mpsc::Receiver<()>,
) -> Result<(), String>
where
    F: Fn(f32) + Send + Sync + 'static,
{
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("No input audio device available")?;

    let config = device
        .default_input_config()
        .map_err(|e| format!("Cannot get default input config: {e}"))?;

    let native_sample_rate = config.sample_rate().0;
    *native_rate.lock().unwrap() = native_sample_rate;
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();

    let stream_config = StreamConfig {
        channels: config.channels(),
        sample_rate: SampleRate(native_sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let samples_clone = Arc::clone(&samples);
    let on_level = Arc::new(on_level);

    let stream: cpal::Stream = match sample_format {
        SampleFormat::F32 => {
            let on_level = Arc::clone(&on_level);
            device
                .build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| {
                        let mono: Vec<i16> = data
                            .chunks(channels)
                            .map(|ch| {
                                let avg = ch.iter().copied().sum::<f32>() / ch.len() as f32;
                                (avg.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                            })
                            .collect();
                        let rms = compute_rms(&mono);
                        on_level(rms);
                        samples_clone.lock().unwrap().extend_from_slice(&mono);
                    },
                    |err| tracing::error!("Audio error: {err}"),
                    Some(Duration::from_secs(5)),
                )
                .map_err(|e| format!("Build stream error: {e}"))?
        }
        SampleFormat::I16 => {
            let on_level = Arc::clone(&on_level);
            device
                .build_input_stream(
                    &stream_config,
                    move |data: &[i16], _| {
                        let mono: Vec<i16> = data
                            .chunks(channels)
                            .map(|ch| {
                                let avg = ch.iter().map(|&s| s as i32).sum::<i32>() / ch.len() as i32;
                                avg as i16
                            })
                            .collect();
                        let rms = compute_rms(&mono);
                        on_level(rms);
                        samples_clone.lock().unwrap().extend_from_slice(&mono);
                    },
                    |err| tracing::error!("Audio error: {err}"),
                    Some(Duration::from_secs(5)),
                )
                .map_err(|e| format!("Build stream error: {e}"))?
        }
        _ => return Err(format!("Unsupported sample format: {:?}", sample_format)),
    };

    stream.play().map_err(|e| format!("Stream play error: {e}"))?;

    // Block until stop signal or is_recording goes false
    loop {
        if stop_rx.try_recv().is_ok() || !is_recording.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    // Stream is dropped here, stopping capture
    Ok(())
}

fn compute_rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples
        .iter()
        .map(|&s| (s as f64 / i16::MAX as f64).powi(2))
        .sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

pub fn resample_linear(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = (samples.len() as f64 / ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let src_idx = src_pos as usize;
        let frac = src_pos - src_idx as f64;
        let s0 = *samples.get(src_idx).unwrap_or(&0) as f64;
        let s1 = *samples.get(src_idx + 1).unwrap_or(&0) as f64;
        out.push((s0 + (s1 - s0) * frac) as i16);
    }
    out
}

/// Start a background wake-word listening loop.
///
/// Captures audio in 0.8s chunks, applies a silence gate (RMS > 0.01),
/// sends non-silent chunks to the transcribe endpoint, and emits
/// `wake-word-detected` on the AppHandle when the configured word is heard.
pub fn start_wake_listener(
    state: &WakeListenerState,
    wake_word: String,
    base_url: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Cancel any existing listener first
    stop_wake_listener(state);

    state.is_running.store(true, Ordering::SeqCst);

    let is_running = Arc::clone(&state.is_running);
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    *state.stop_tx.lock().unwrap() = Some(stop_tx);

    thread::spawn(move || {
        let result = wake_listener_thread(app, wake_word, base_url, is_running.clone(), stop_rx);
        is_running.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            tracing::error!("Wake listener thread error: {e}");
        }
    });

    Ok(())
}

/// Stop the background wake-word listening loop.
pub fn stop_wake_listener(state: &WakeListenerState) {
    if let Some(tx) = state.stop_tx.lock().unwrap().take() {
        let _ = tx.send(());
    }
    state.is_running.store(false, Ordering::SeqCst);
}

/// The OS thread that drives passive wake-word detection.
///
/// Uses a two-stage, ultra-low-power design:
///
///   Stage 1 — VAD (Voice Activity Detection)
///     Drains 30 ms frames from the ring buffer, computes RMS energy, and
///     runs a tiny state machine (SILENCE → ONSET → SPEECH → SILENCE).
///     No network, no allocation beyond the frame drain.  CPU ≈ 0%.
///
///   Stage 2 — ASR (only on speech end)
///     When the VAD transitions back to SILENCE after confirmed speech, the
///     accumulated utterance is resampled to 16 kHz, encoded as WAV, and
///     sent to the transcribe endpoint exactly once per spoken phrase.
///     Typical call rate: 2–5 / min vs. the old blind 75 / min.
fn wake_listener_thread(
    app: tauri::AppHandle,
    wake_word: String,
    base_url: String,
    is_running: Arc<AtomicBool>,
    stop_rx: std::sync::mpsc::Receiver<()>,
) -> Result<(), String> {
    // ── VAD tuning ──────────────────────────────────────────────────────────
    /// Frame poll interval — matches WebRTC VAD frame size.
    const FRAME_MS: u64 = 30;

    /// RMS above this → speech onset candidate (two-threshold hysteresis).
    const SPEECH_RMS: f32 = 0.015;

    /// RMS below this → silence (lower than SPEECH_RMS to prevent flapping).
    const SILENCE_RMS: f32 = 0.008;

    /// Consecutive above-threshold frames required to confirm speech started.
    /// 3 × 30 ms = 90 ms — short enough not to miss word beginnings.
    const ONSET_FRAMES: u32 = 3;

    /// Consecutive below-threshold frames before speech is declared ended.
    /// 12 × 30 ms = 360 ms tail — natural inter-word pause tolerance.
    const TAIL_FRAMES: u32 = 12;

    /// Hard cap on accumulated speech (at native rate) before a forced ASR
    /// check.  Safety valve so the buffer never grows without bound.
    /// 3 s × max_hardware_rate (96 kHz) ≈ 288 000 samples.
    const MAX_SPEECH_SAMPLES: usize = 300_000;

    // ── cpal device setup ───────────────────────────────────────────────────
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("No input audio device available")?;
    let config = device
        .default_input_config()
        .map_err(|e| format!("Cannot get default input config: {e}"))?;

    let native_rate = config.sample_rate().0;
    let channels    = config.channels() as usize;
    let sample_fmt  = config.sample_format();

    // Ring buffer written by the cpal callback, drained by the VAD loop.
    let ring: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::new()));
    let ring_fill   = Arc::clone(&ring);

    let stream_cfg = cpal::StreamConfig {
        channels:    config.channels(),
        sample_rate: cpal::SampleRate(native_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // Build the input stream — converts to mono i16 regardless of hw format.
    let stream: cpal::Stream = match sample_fmt {
        cpal::SampleFormat::F32 => device
            .build_input_stream(
                &stream_cfg,
                move |data: &[f32], _| {
                    let mono: Vec<i16> = data
                        .chunks(channels)
                        .map(|ch| {
                            let avg = ch.iter().copied().sum::<f32>() / ch.len() as f32;
                            (avg.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                        })
                        .collect();
                    ring_fill.lock().unwrap().extend_from_slice(&mono);
                },
                |err| tracing::error!("Wake listener audio error: {err}"),
                Some(Duration::from_secs(5)),
            )
            .map_err(|e| format!("Build stream error: {e}"))?,
        cpal::SampleFormat::I16 => device
            .build_input_stream(
                &stream_cfg,
                move |data: &[i16], _| {
                    let mono: Vec<i16> = data
                        .chunks(channels)
                        .map(|ch| {
                            let avg = ch
                                .iter()
                                .map(|&s| s as i32)
                                .sum::<i32>()
                                / ch.len() as i32;
                            avg as i16
                        })
                        .collect();
                    ring_fill.lock().unwrap().extend_from_slice(&mono);
                },
                |err| tracing::error!("Wake listener audio error: {err}"),
                Some(Duration::from_secs(5)),
            )
            .map_err(|e| format!("Build stream error: {e}"))?,
        _ => return Err(format!("Unsupported sample format: {:?}", sample_fmt)),
    };
    stream.play().map_err(|e| format!("Stream play error: {e}"))?;

    // ── HTTP client (re-used across ASR calls) ──────────────────────────────
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    // Normalise the wake word — strip punctuation so it matches whisper output
    // e.g. "Hey Goose" → "hey goose" (whisper returns "Hey, Goose." which normalises to "hey  goose")
    let wake_lower: String = wake_word.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect();

    // ── VAD state ───────────────────────────────────────────────────────────
    let mut onset_frames:   u32      = 0;  // consecutive above-threshold frames
    let mut tail_frames:    u32      = 0;  // consecutive silence frames while in speech
    let mut in_speech:      bool     = false;
    let mut speech_buf:     Vec<i16> = Vec::new();

    // ── Main loop ───────────────────────────────────────────────────────────
    loop {
        if stop_rx.try_recv().is_ok() || !is_running.load(Ordering::SeqCst) {
            break;
        }

        thread::sleep(Duration::from_millis(FRAME_MS));

        // Drain the ring buffer for this 30 ms window.
        let frame: Vec<i16> = {
            let mut buf = ring.lock().unwrap();
            buf.drain(..).collect()
        };
        if frame.is_empty() {
            continue;
        }

        let rms = compute_rms(&frame);

        // ── Stage 1: VAD state machine (pure math, no network) ──────────────
        if !in_speech {
            // SILENCE / ONSET state
            if rms >= SPEECH_RMS {
                onset_frames += 1;
                speech_buf.extend_from_slice(&frame); // keep pre-roll
                if onset_frames >= ONSET_FRAMES {
                    // Confirmed speech — transition to SPEECH state
                    in_speech    = true;
                    tail_frames  = 0;
                    tracing::debug!("VAD → SPEECH ({} pre-roll samples)", speech_buf.len());
                }
            } else {
                // Not loud enough — discard pre-roll and reset counter
                onset_frames = 0;
                speech_buf.clear();
            }
        } else {
            // SPEECH state — accumulate frame
            speech_buf.extend_from_slice(&frame);

            if rms < SILENCE_RMS {
                tail_frames += 1;
            } else {
                tail_frames = 0; // speech resumed — reset tail
            }

            let end_of_speech  = tail_frames >= TAIL_FRAMES;
            let buffer_maxed   = speech_buf.len() >= MAX_SPEECH_SAMPLES;

            if end_of_speech || buffer_maxed {
                tracing::debug!(
                    "VAD → ASR ({} samples, forced={})",
                    speech_buf.len(),
                    buffer_maxed,
                );

                // ── Stage 2: ASR (one call per utterance) ───────────────────
                let samples_16k = if native_rate != 16000 {
                    resample_linear(&speech_buf, native_rate, 16000)
                } else {
                    speech_buf.clone()
                };

                'asr: {
                    let wav = match encode_wav(&samples_16k, 16000) {
                        Ok(w) => w,
                        Err(e) => {
                            tracing::warn!("Wake VAD WAV encode failed: {e}");
                            break 'asr;
                        }
                    };
                    let part = match reqwest::blocking::multipart::Part::bytes(wav)
                        .file_name("wake.wav")
                        .mime_str("audio/wav")
                    {
                        Ok(p) => p,
                        Err(_) => break 'asr,
                    };
                    let form = reqwest::blocking::multipart::Form::new().part("audio", part);
                    let res = match client
                        .post(format!("{}/api/v1/transcribe", base_url))
                        .multipart(form)
                        .send()
                    {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::debug!("Wake ASR request failed: {e}");
                            break 'asr;
                        }
                    };
                    if !res.status().is_success() {
                        break 'asr;
                    }

                    #[derive(serde::Deserialize)]
                    struct Tr { text: String }
                    if let Ok(t) = res.json::<Tr>() {
                        // Strip punctuation so "Hey, Goose." matches "hey goose"
                        let transcript: String = t.text.to_lowercase()
                            .chars()
                            .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
                            .collect();
                        tracing::debug!("Wake ASR: {:?}", transcript);
                        if transcript.contains(&wake_lower) {
                            tracing::info!("Wake word '{}' detected", wake_word);
                            let _ = app.emit("wake-word-detected", ());
                            return Ok(()); // exit thread — recording takes over
                        }
                    }
                }

                // Reset VAD state for next utterance
                speech_buf.clear();
                onset_frames = 0;
                tail_frames  = 0;
                in_speech    = false;
            }
        }
    }

    Ok(())
}

pub fn encode_wav(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>, String> {
    let data_bytes = samples.len() * 2;
    let mut buf = Vec::with_capacity(44 + data_bytes);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for &s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    Ok(buf)
}
