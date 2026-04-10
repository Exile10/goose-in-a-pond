//! Qwen TTS adapter — implements [`VoiceOutput`] by calling a local
//! OpenAI-compatible TTS HTTP server (`POST /v1/audio/speech`).
//!
//! The server is the embedded `qwen_tts_serve.py` script in `pond-server`,
//! which wraps the `qwen-tts` Python package (Qwen3-TTS).  The script is
//! written to disk and spawned automatically via `qwen_tts_process::try_start`.
//!
//! The server accepts:
//! ```json
//! POST /v1/audio/speech
//! {"model": "qwen3-tts", "input": "Hello world", "voice": "Vivian"}
//! ```
//! and returns raw WAV bytes.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pond_core::ports::voice_output::VoiceOutput;

/// Default server address for the Qwen TTS HTTP server.
pub const DEFAULT_HOST: &str = "http://127.0.0.1:8181";
/// Default voice name for Qwen3-TTS CustomVoice.
pub const DEFAULT_VOICE: &str = "Vivian";
/// Model identifier sent in the request body (informational; server ignores it).
pub const DEFAULT_MODEL: &str = "qwen3-tts";

/// TTS adapter that calls a local OpenAI-compatible speech server.
pub struct QwenTtsOutput {
    client:     reqwest::Client,
    /// Full URL to the `/v1/audio/speech` endpoint.
    speech_url: String,
    voice:      String,
    model:      String,
}

impl QwenTtsOutput {
    /// Create a new adapter pointing at `server_url` (defaults to [`DEFAULT_HOST`]).
    pub fn new(server_url: Option<&str>) -> Self {
        let base = server_url.unwrap_or(DEFAULT_HOST).trim_end_matches('/');
        Self {
            client:     reqwest::Client::new(),
            speech_url: format!("{}/v1/audio/speech", base),
            voice:      DEFAULT_VOICE.to_string(),
            model:      DEFAULT_MODEL.to_string(),
        }
    }

    /// Override the TTS voice (e.g. "Vivian", "Chelsie").
    pub fn with_voice(mut self, voice: &str) -> Self {
        self.voice = voice.to_string();
        self
    }

    /// Override the model identifier sent in the request.
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    /// POST to the TTS server and return the raw audio bytes.
    async fn fetch_audio(&self, text: &str) -> Result<Vec<u8>> {
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
            "voice": self.voice,
        });

        let resp = self
            .client
            .post(&self.speech_url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("qwen-tts: failed to reach {}", self.speech_url))?;

        if !resp.status().is_success() {
            return Err(anyhow!(
                "qwen-tts: server returned {} for speech request",
                resp.status()
            ));
        }

        let bytes = resp
            .bytes()
            .await
            .context("qwen-tts: failed to read audio response")?;

        Ok(bytes.to_vec())
    }
}

#[async_trait]
impl VoiceOutput for QwenTtsOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        let audio = self.fetch_audio(text).await?;

        // Decode and play on the blocking thread pool — rodio is not async.
        tokio::task::spawn_blocking(move || play_audio(audio))
            .await
            .context("qwen-tts: audio playback task panicked")??;

        Ok(())
    }
}

/// Decode WAV/MP3 bytes and play them through the default audio output.
fn play_audio(bytes: Vec<u8>) -> Result<()> {
    use rodio::{Decoder, OutputStream, Sink};
    use std::io::Cursor;

    let cursor = Cursor::new(bytes);
    let (_stream, handle) =
        OutputStream::try_default().context("qwen-tts: no audio output device available")?;
    let sink = Sink::try_new(&handle).context("qwen-tts: failed to create audio sink")?;
    let source = Decoder::new(cursor).context("qwen-tts: failed to decode audio")?;
    sink.append(source);
    sink.sleep_until_end();
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Minimal valid WAV: 44-byte header + 1 sample of silence.
    fn minimal_wav() -> Vec<u8> {
        let mut wav = Vec::new();
        // RIFF header
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes()); // chunk size = 36 + data size
        wav.extend_from_slice(b"WAVE");
        // fmt sub-chunk
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // sub-chunk size
        wav.extend_from_slice(&1u16.to_le_bytes());  // PCM
        wav.extend_from_slice(&1u16.to_le_bytes());  // mono
        wav.extend_from_slice(&22050u32.to_le_bytes()); // sample rate
        wav.extend_from_slice(&44100u32.to_le_bytes()); // byte rate
        wav.extend_from_slice(&2u16.to_le_bytes());  // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        // data sub-chunk
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&2u32.to_le_bytes()); // 1 sample = 2 bytes
        wav.extend_from_slice(&0u16.to_le_bytes()); // silent sample
        wav
    }

    #[tokio::test]
    async fn sends_correct_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(minimal_wav()),
            )
            .mount(&server)
            .await;

        let adapter = QwenTtsOutput::new(Some(&server.uri()))
            .with_voice("TestVoice");

        // fetch_audio is enough to verify body — no real audio device needed.
        let audio = adapter.fetch_audio("Hello world").await.unwrap();
        assert!(!audio.is_empty(), "should receive audio bytes");

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["input"], "Hello world");
        assert_eq!(body["voice"], "TestVoice");
        assert_eq!(body["model"], DEFAULT_MODEL);
    }

    #[tokio::test]
    async fn returns_error_on_server_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let adapter = QwenTtsOutput::new(Some(&server.uri()));
        let err = adapter.fetch_audio("hello").await.unwrap_err();
        assert!(
            err.to_string().contains("500"),
            "expected 500 in error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn returns_error_when_server_offline() {
        // Port 1 is reserved — instant connection refused.
        let adapter = QwenTtsOutput::new(Some("http://127.0.0.1:1"));
        let err = adapter.fetch_audio("hi").await.unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("qwen"),
            "expected 'qwen' in error message, got: {}",
            err
        );
    }

    #[test]
    fn default_url_and_voice() {
        let adapter = QwenTtsOutput::new(None);
        assert!(adapter.speech_url.contains("8181"));
        assert_eq!(adapter.voice, DEFAULT_VOICE);
    }

    #[test]
    fn with_voice_overrides_default() {
        let adapter = QwenTtsOutput::new(None).with_voice("alloy");
        assert_eq!(adapter.voice, "alloy");
    }
}
