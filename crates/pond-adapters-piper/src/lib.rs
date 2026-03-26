//! Piper TTS adapter for Goose In A Pond.
//!
//! Implements the `VoiceOutput` port by:
//!   1. Spawning `piper --model <model> --output-raw --quiet`
//!   2. Writing the text to piper's stdin, then closing it
//!   3. Reading raw 16-bit PCM from piper's stdout
//!   4. Wrapping the PCM in a minimal WAV header
//!   5. Playing the WAV through the default audio output via `rodio`
//!
//! Piper is a fast, local neural TTS engine.  The `en_US-lessac-medium`
//! voice outputs mono 16-bit PCM at 22 050 Hz.  Run `pond-server setup`
//! to download the binary and voice model automatically.
//!
//! # Usage
//! ```no_run
//! use pond_adapters_piper::PiperOutput;
//! use std::path::PathBuf;
//!
//! let tts = PiperOutput::new(
//!     PathBuf::from("/data/bin/piper"),
//!     PathBuf::from("/data/models/tts/en_US-lessac-medium.onnx"),
//! );
//! ```

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use pond_core::ports::voice_output::VoiceOutput;
use std::io::Write as _;
use std::path::PathBuf;

// ── PiperOutput ───────────────────────────────────────────────────────────────

/// VoiceOutput adapter that synthesises speech via the Piper TTS subprocess.
pub struct PiperOutput {
    piper_bin: PathBuf,
    model: PathBuf,
    /// Sample rate of the model's raw PCM output.
    /// `en_US-lessac-medium` = 22 050 Hz.  Override with `with_sample_rate()`.
    sample_rate: u32,
}

impl PiperOutput {
    /// Create a new `PiperOutput`.
    ///
    /// `piper_bin` — path to the piper executable.
    /// `model`     — path to the `.onnx` voice model file.
    pub fn new(piper_bin: PathBuf, model: PathBuf) -> Self {
        Self {
            piper_bin,
            model,
            sample_rate: 22_050,
        }
    }

    /// Override the expected sample rate (default: 22 050 for lessac-medium).
    pub fn with_sample_rate(mut self, sample_rate: u32) -> Self {
        self.sample_rate = sample_rate;
        self
    }

    /// Assemble the piper command arguments (useful for tests without a real binary).
    pub fn build_args(&self) -> Vec<String> {
        vec![
            "--model".to_string(),
            self.model.to_string_lossy().to_string(),
            "--output-raw".to_string(),
            "--quiet".to_string(),
        ]
    }
}

#[async_trait]
impl VoiceOutput for PiperOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        let bin = self.piper_bin.clone();
        let model = self.model.clone();
        let sample_rate = self.sample_rate;
        let text = text.to_string();

        // Piper is a blocking subprocess — run it off the async executor.
        tokio::task::spawn_blocking(move || {
            speak_blocking(&bin, &model, sample_rate, &text)
        })
        .await
        .context("piper speak task panicked")??;

        Ok(())
    }
}

// ── Blocking implementation ───────────────────────────────────────────────────

fn speak_blocking(
    piper_bin: &std::path::Path,
    model: &std::path::Path,
    sample_rate: u32,
    text: &str,
) -> Result<()> {
    use std::process::{Command, Stdio};

    // Spawn piper, pipe stdin + stdout.
    let mut child = Command::new(piper_bin)
        .args(["--model", &model.to_string_lossy()])
        .args(["--output-raw", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn piper at {}", piper_bin.display()))?;

    // Write text to stdin and close it so piper knows there is no more input.
    {
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("piper stdin unavailable"))?;
        stdin
            .write_all(text.as_bytes())
            .context("Failed to write text to piper stdin")?;
        // `stdin` dropped here → EOF signalled to piper
    }

    // Read all raw PCM from stdout.
    let output = child.wait_with_output().context("Failed to wait for piper")?;

    if !output.status.success() {
        return Err(anyhow!(
            "piper exited with status {}",
            output.status
        ));
    }

    let pcm = output.stdout;
    if pcm.is_empty() {
        tracing::warn!("piper produced no PCM output for text: {:?}", text);
        return Ok(());
    }

    // Wrap raw PCM in a WAV container so rodio can decode it.
    let wav = pcm_to_wav(&pcm, sample_rate);

    // Play through the default audio output device.
    play_wav(wav)
}

// ── WAV encoder ───────────────────────────────────────────────────────────────

/// Wrap raw 16-bit mono PCM in a minimal RIFF/WAV container.
fn pcm_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
    let block_align = channels * bits_per_sample / 8;
    let data_len = pcm.len() as u32;
    let riff_len = 36 + data_len;

    let mut wav = Vec::with_capacity(44 + pcm.len());
    // RIFF chunk
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    // fmt  sub-chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());          // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes());           // PCM format
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    // data sub-chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

// ── Audio playback ────────────────────────────────────────────────────────────

fn play_wav(wav: Vec<u8>) -> Result<()> {
    use rodio::{Decoder, OutputStream, Sink};
    use std::io::Cursor;

    let cursor = Cursor::new(wav);
    let decoder = Decoder::new(cursor).context("Failed to decode WAV for playback")?;

    let (_stream, stream_handle) =
        OutputStream::try_default().context("No audio output device found")?;
    let sink = Sink::try_new(&stream_handle).context("Failed to create audio sink")?;

    sink.append(decoder);
    sink.sleep_until_end();

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piper_args_include_model_and_flags() {
        let tts = PiperOutput::new(
            PathBuf::from("/data/bin/piper"),
            PathBuf::from("/data/models/tts/en_US-lessac-medium.onnx"),
        );
        let args = tts.build_args();
        assert_eq!(args[0], "--model");
        assert!(args[1].contains("en_US-lessac-medium.onnx"));
        assert!(args.contains(&"--output-raw".to_string()));
        assert!(args.contains(&"--quiet".to_string()));
    }

    #[test]
    fn with_sample_rate_overrides_default() {
        let tts = PiperOutput::new(PathBuf::from("piper"), PathBuf::from("model.onnx"))
            .with_sample_rate(16_000);
        assert_eq!(tts.sample_rate, 16_000);
    }

    #[test]
    fn pcm_to_wav_header_is_correct() {
        // 2 bytes of PCM (one 16-bit sample at 22050 Hz mono)
        let pcm = vec![0x01u8, 0x00u8];
        let wav = pcm_to_wav(&pcm, 22_050);

        // RIFF magic
        assert_eq!(&wav[0..4], b"RIFF");
        // WAVE magic
        assert_eq!(&wav[8..12], b"WAVE");
        // fmt  chunk ID
        assert_eq!(&wav[12..16], b"fmt ");
        // data chunk ID
        assert_eq!(&wav[36..40], b"data");
        // data length
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len as usize, pcm.len());
        // PCM payload
        assert_eq!(&wav[44..], pcm.as_slice());
    }

    #[test]
    fn piper_output_compiles_as_voice_output() {
        use pond_core::ports::voice_output::VoiceOutput;
        use std::sync::Arc;
        let _out: Arc<dyn VoiceOutput> = Arc::new(PiperOutput::new(
            PathBuf::from("piper"),
            PathBuf::from("model.onnx"),
        ));
    }
}
