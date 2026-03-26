//! Whisper ASR adapter for Goose In A Pond.
//!
//! Exports:
//! - `WhisperInput`           — `VoiceInput` port: record mic → whisper → text
//! - `WhisperKeywordDetector` — `WakeWordDetector` port: poll mic until trigger phrase heard
//!
//! Implements the `VoiceInput` port by:
//!   1. Recording audio from the default microphone via `cpal`
//!   2. Encoding the captured PCM as a WAV file in memory
//!   3. POSTing the WAV to a local [whisper.cpp server] at `POST /inference`
//!      (the whisper.cpp server API — not the OpenAI `/v1/audio/transcriptions` path)
//!   4. Returning the transcribed text
//!
//! This keeps the same "local HTTP server" pattern as `pond-adapters-llamafile`
//! — no native Rust bindings, no long compile times.
//!
//! ## Running the whisper.cpp server
//!
//! ```bash
//! # Download a model (e.g. ggml-base.en.bin from huggingface)
//! ./server -m models/ggml-base.en.bin --port 9000
//! ```
//!
//! [whisper.cpp server]: https://github.com/ggerganov/whisper.cpp/tree/master/examples/server

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use pond_core::ports::voice_input::VoiceInput;
use pond_core::ports::wake_word::WakeWordDetector;
use std::sync::{Arc, Mutex};

/// Default whisper.cpp server URL.
pub const DEFAULT_HOST: &str = "http://127.0.0.1:9000";

// ── WhisperInput ─────────────────────────────────────────────────────────────

/// VoiceInput adapter that records from the microphone and transcribes via
/// a local whisper.cpp HTTP server.
pub struct WhisperInput {
    client: reqwest::Client,
    transcription_url: String,
    /// How long to record before sending for transcription (seconds).
    duration_secs: u32,
}

impl WhisperInput {
    /// Create a new adapter pointing at `server_url` (e.g. `"http://127.0.0.1:9000"`).
    /// Defaults to `DEFAULT_HOST` when `server_url` is `None`.
    pub fn new(server_url: Option<&str>) -> Self {
        let base = server_url.unwrap_or(DEFAULT_HOST);
        Self {
            client: reqwest::Client::new(),
            transcription_url: format!("{}/inference", base),
            duration_secs: 5,
        }
    }

    /// Override the recording duration (default: 5 seconds).
    pub fn with_duration(mut self, secs: u32) -> Self {
        self.duration_secs = secs;
        self
    }
}

#[async_trait]
impl VoiceInput for WhisperInput {
    async fn listen(&self) -> Result<Option<String>> {
        let duration = self.duration_secs;

        // Audio capture is blocking — run it in a dedicated thread.
        let wav_bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            println!("  🎤 Recording for {} seconds...", duration);
            let (samples, sample_rate) = record_mono_f32(duration)?;
            if samples.is_empty() {
                return Err(anyhow!("No audio captured"));
            }
            // Resample to 16 kHz (whisper's expected rate) then encode WAV.
            let samples_16k = resample_to_16k(&samples, sample_rate);
            Ok(encode_wav_mono_16k(&samples_16k))
        })
        .await??;

        self.transcribe_wav(wav_bytes).await
    }

    fn prompt(&self) -> &str {
        "🎤 "
    }
}

impl WhisperInput {
    /// POST pre-encoded WAV bytes to the whisper server and return the transcript.
    ///
    /// Exposed publicly so callers can transcribe audio from any source (e.g.
    /// a file on disk) without going through microphone capture.
    pub async fn transcribe_wav(&self, wav_bytes: Vec<u8>) -> Result<Option<String>> {
        let part = reqwest::multipart::Part::bytes(wav_bytes)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| anyhow!("MIME error: {}", e))?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "json");

        let resp = self
            .client
            .post(&self.transcription_url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| anyhow!("whisper server request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("whisper server error {}: {}", status, body));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow!("whisper response parse error: {}", e))?;

        let text = json["text"].as_str().unwrap_or("").trim().to_string();

        Ok(if text.is_empty() { None } else { Some(text) })
    }
}

// ── Audio capture ─────────────────────────────────────────────────────────────

/// Record `duration_secs` seconds of audio from the default input device.
/// Returns interleaved-to-mono f32 PCM samples and the device's sample rate.
fn record_mono_f32(duration_secs: u32) -> Result<(Vec<f32>, u32)> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("No audio input device found"))?;

    let config = device
        .default_input_config()
        .map_err(|e| anyhow!("Failed to get input config: {}", e))?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let samples_writer = Arc::clone(&samples);

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Downmix multi-channel to mono by averaging
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                    .collect();
                samples_writer.lock().unwrap().extend_from_slice(&mono);
            },
            |e| eprintln!("  ⚠ Audio stream error: {}", e),
            None,
        )?,
        cpal::SampleFormat::I16 => {
            device.build_input_stream(
                &config.into(),
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|frame| {
                            let sum: f32 = frame
                                .iter()
                                .map(|&s| s as f32 / i16::MAX as f32)
                                .sum();
                            sum / channels as f32
                        })
                        .collect();
                    samples_writer.lock().unwrap().extend_from_slice(&mono);
                },
                |e| eprintln!("  ⚠ Audio stream error: {}", e),
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            device.build_input_stream(
                &config.into(),
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|frame| {
                            let sum: f32 = frame
                                .iter()
                                .map(|&s| s as f32 / u16::MAX as f32 * 2.0 - 1.0)
                                .sum();
                            sum / channels as f32
                        })
                        .collect();
                    samples_writer.lock().unwrap().extend_from_slice(&mono);
                },
                |e| eprintln!("  ⚠ Audio stream error: {}", e),
                None,
            )?
        }
        fmt => return Err(anyhow!("Unsupported audio sample format: {:?}", fmt)),
    };

    stream
        .play()
        .map_err(|e| anyhow!("Failed to start audio stream: {}", e))?;
    std::thread::sleep(std::time::Duration::from_secs(duration_secs as u64));
    drop(stream); // stops recording

    // Recover the Vec — safe because the stream callback is stopped after drop
    let recorded = match Arc::try_unwrap(samples) {
        Ok(mutex) => mutex.into_inner().unwrap(),
        Err(arc) => arc.lock().unwrap().clone(),
    };

    Ok((recorded, sample_rate))
}

// ── DSP helpers ───────────────────────────────────────────────────────────────

/// Linear interpolation resample to 16 000 Hz (whisper's expected rate).
fn resample_to_16k(samples: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate == 16_000 {
        return samples.to_vec();
    }
    let ratio = 16_000.0_f64 / src_rate as f64;
    let new_len = (samples.len() as f64 * ratio) as usize;
    (0..new_len)
        .map(|i| {
            let src = i as f64 / ratio;
            let lo = src.floor() as usize;
            let hi = (lo + 1).min(samples.len().saturating_sub(1));
            let frac = (src - src.floor()) as f32;
            samples[lo] * (1.0 - frac) + samples[hi] * frac
        })
        .collect()
}

// ── WAV encoding ──────────────────────────────────────────────────────────────

/// Encode mono 16-bit PCM at 16 kHz as a WAV byte vector.
/// Avoids any external WAV crate dependency.
fn encode_wav_mono_16k(samples: &[f32]) -> Vec<u8> {
    let sample_rate: u32 = 16_000;
    let channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;
    let data_len = (samples.len() * 2) as u32; // 2 bytes per i16 sample

    let mut buf = Vec::with_capacity(44 + data_len as usize);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());

    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());

    for &s in samples {
        let sample = (s.clamp(-1.0, 1.0) * 32_767.0) as i16;
        buf.extend_from_slice(&sample.to_le_bytes());
    }

    buf
}

// ── WhisperKeywordDetector ────────────────────────────────────────────────────

/// WakeWordDetector that polls the microphone until the trigger phrase is heard.
///
/// Records short clips (default 2 s), transcribes each via whisper.cpp, and
/// returns `Ok(())` as soon as the transcript contains the trigger word/phrase
/// (case-insensitive).
///
/// This is a polyfill for real-time KWS.  It has ~2 s latency per poll cycle
/// and requires the whisper server to be running.  On Jetson with the `tiny`
/// model, one cycle takes roughly 2 s record + 0.5 s inference = 2.5 s.
///
/// A dedicated native KWS library (Porcupine, Vosk KWS) will replace this
/// when always-on wake-word detection is required.
pub struct WhisperKeywordDetector {
    whisper: WhisperInput,
    /// The trigger phrase to listen for (case-insensitive substring match).
    trigger: String,
}

impl WhisperKeywordDetector {
    /// Create a detector listening for `trigger` (e.g. `"goose"`).
    /// Uses `server_url` for whisper (defaults to `DEFAULT_HOST`).
    /// Records 2-second clips by default.
    pub fn new(server_url: Option<&str>, trigger: impl Into<String>) -> Self {
        Self {
            whisper: WhisperInput::new(server_url).with_duration(2),
            trigger: trigger.into().to_lowercase(),
        }
    }
}

#[async_trait]
impl WakeWordDetector for WhisperKeywordDetector {
    async fn wait_for_activation(&self) -> Result<()> {
        loop {
            match self.whisper.listen().await {
                Ok(Some(text)) if text.to_lowercase().contains(&self.trigger) => {
                    tracing::info!("Wake word detected: \"{}\"", text.trim());
                    return Ok(());
                }
                Ok(_) => {
                    // Nothing heard or trigger not in transcript — keep polling
                    tracing::debug!("No wake word, polling again...");
                }
                Err(e) => {
                    // Log but keep polling — a single failed clip is not fatal
                    tracing::warn!("Wake word poll error (retrying): {}", e);
                }
            }
        }
    }

    fn activation_prompt(&self) -> &str {
        "Say \"Goose\" to activate..."
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_input_default_prompt() {
        assert_eq!(WhisperInput::new(None).prompt(), "🎤 ");
    }

    #[test]
    fn whisper_input_custom_url() {
        let w = WhisperInput::new(Some("http://192.168.1.100:9000"));
        assert!(w.transcription_url.starts_with("http://192.168.1.100:9000"));
    }

    #[test]
    fn encode_wav_has_riff_header() {
        let samples = vec![0.0f32; 160]; // 10ms of silence
        let wav = encode_wav_mono_16k(&samples);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
    }

    #[test]
    fn encode_wav_correct_data_length() {
        let n = 100usize;
        let samples = vec![0.5f32; n];
        let wav = encode_wav_mono_16k(&samples);
        // 44-byte header + n * 2 bytes of PCM data
        assert_eq!(wav.len(), 44 + n * 2);
    }

    #[test]
    fn resample_passthrough_at_16k() {
        let samples = vec![1.0f32, 0.5, 0.0];
        let out = resample_to_16k(&samples, 16_000);
        assert_eq!(out, samples);
    }

    #[test]
    fn resample_halves_length_at_32k() {
        let samples: Vec<f32> = (0..64).map(|i| i as f32 / 63.0).collect();
        let out = resample_to_16k(&samples, 32_000);
        // 64 samples @ 32kHz → ~32 samples @ 16kHz
        assert!((out.len() as i32 - 32).abs() <= 1, "len was {}", out.len());
    }

    #[test]
    fn duration_builder() {
        let w = WhisperInput::new(None).with_duration(10);
        assert_eq!(w.duration_secs, 10);
    }

    /// Verify the full PCM pipeline against a real audio file.
    ///
    /// jfk.wav is a 16-bit mono 16 kHz PCM WAV (the canonical whisper.cpp sample).
    /// We parse its samples, run them through our DSP helpers, and re-encode — then
    /// verify the resulting WAV is structurally valid and the right length.
    #[test]
    fn jfk_wav_round_trips_through_dsp_pipeline() {
        let wav_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/blobs/jfk.wav");
        let wav_bytes = std::fs::read(&wav_path)
            .expect("tests/blobs/jfk.wav not found — run from workspace root");

        assert!(wav_bytes.len() > 44, "WAV file too short");

        // Locate "data" chunk (handles any non-standard pre-data chunks)
        let data_offset = wav_bytes
            .windows(4)
            .position(|w| w == b"data")
            .expect("no 'data' chunk in jfk.wav")
            + 8; // skip "data" tag (4) + chunk-size field (4)

        let pcm_bytes = &wav_bytes[data_offset..];
        let samples: Vec<f32> = pcm_bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_767.0)
            .collect();

        assert!(!samples.is_empty(), "jfk.wav has no PCM samples");

        // jfk.wav is already 16 kHz → resample is a passthrough
        let resampled = resample_to_16k(&samples, 16_000);
        assert_eq!(
            resampled.len(),
            samples.len(),
            "passthrough resample changed length"
        );

        // Encode → verify WAV structure
        let encoded = encode_wav_mono_16k(&resampled);
        assert_eq!(&encoded[0..4], b"RIFF", "missing RIFF marker");
        assert_eq!(&encoded[8..12], b"WAVE", "missing WAVE marker");
        assert_eq!(
            encoded.len(),
            44 + samples.len() * 2,
            "encoded length mismatch"
        );
    }
}
