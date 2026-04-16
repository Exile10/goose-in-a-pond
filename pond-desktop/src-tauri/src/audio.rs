/// Audio capture module.
///
/// cpal::Stream is !Send so it cannot be stored in Tauri managed state directly.
/// Instead we keep only Arc/AtomicBool in managed state and run the cpal stream
/// on a dedicated OS thread that lives as long as recording is active.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
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

fn resample_linear(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
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
