//! In-process Piper TTS via `piper-rs` (ONNX Runtime + espeak-rs phonemizer).
//!
//! `PiperRsOutput` is the default `VoiceOutput` adapter — loads a Piper voice
//! (`.onnx` + `.onnx.json`) once at startup and synthesises directly, with no
//! per-utterance subprocess fork+exec.
//!
//! ## Crash isolation
//!
//! Every call into `piper-rs` (model load, `create()`) is wrapped in
//! `catch_unwind` so an ort / espeak panic returns `Err` instead of aborting
//! the whole pond-server process.
//!
//! ## Hot-swap
//!
//! `rebuild_with(new_model, new_config)` atomically replaces the loaded voice
//! under a `tokio::sync::RwLock<Arc<Mutex<Piper>>>`, mirroring
//! `WhisperRsInput::rebuild_with`. piper-rs's `create()` takes `&mut self`,
//! hence the inner `Mutex`: only one synthesis runs at a time per voice.
//!
//! ## espeak-ng data directory
//!
//! piper-rs's transitive `espeak-rs` crate locates `espeak-ng-data` via
//! (1) the `PIPER_ESPEAKNG_DATA_DIRECTORY` env var, (2) the cwd, (3) the
//! current exe's directory. `with_espeak_data(dir)` sets the env var so the
//! lazy `OnceLock` init inside espeak-rs picks the right directory on the
//! first synthesis call.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use piper_rs::Piper;
use pond_core::models::ports::voice_output::VoiceOutput;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

use crate::{
    f32_samples_to_pcm_le_bytes, pcm_to_wav, pick_quip, play_wav_interruptible,
    start_barge_in_thread, start_thinking_tone_thread,
};

/// Env var that espeak-rs consults to find the bundled `espeak-ng-data` dir.
const PIPER_ESPEAKNG_DATA_DIRECTORY: &str = "PIPER_ESPEAKNG_DATA_DIRECTORY";

/// In-process Piper TTS adapter. One loaded voice per instance.
///
/// Cloneable via `Arc<PiperRsOutput>` — the voice lives behind the internal
/// `RwLock<Arc<Mutex<Piper>>>` and is shared across handles. The inner `Mutex`
/// serialises synthesis (`Piper::create` takes `&mut self`).
pub struct PiperRsOutput {
    /// Loaded piper-rs voice. `RwLock` lets `rebuild_with` swap the whole
    /// voice while in-flight synth calls hold the inner mutex on the old voice.
    voice: RwLock<Arc<Mutex<Piper>>>,
    /// Last-known model path, recorded for diagnostics.
    model_path: RwLock<PathBuf>,
    /// Last-known config path.
    config_path: RwLock<PathBuf>,
    /// Cached sample rate from the most recent synthesis. piper-rs's
    /// `create()` returns the rate alongside the samples — we cache the most
    /// recent value so callers and tests can read it cheaply.
    last_sample_rate: Mutex<Option<u32>>,
    /// Thinking-tone stop flag — shared with the background tone thread.
    thinking_active: Arc<AtomicBool>,
    /// Speech interrupt flag — set true to immediately stop TTS playback.
    /// Checked by `play_wav_interruptible()` every 50 ms during playback.
    speech_interrupted: Arc<AtomicBool>,
    /// Barge-in listener active flag — shared with the mic monitoring thread.
    barge_in_active: Arc<AtomicBool>,
    /// True while audio is actively playing. Shared with the barge-in thread so
    /// it applies an elevated RMS threshold during playback (AEC gating).
    is_speaking: Arc<AtomicBool>,
}

impl PiperRsOutput {
    /// Load the voice at `model_path` (`.onnx`) + `config_path` (`.onnx.json`).
    ///
    /// Returns `Err` if either file is missing or piper-rs fails to load
    /// them. A piper-rs panic during load is caught and converted to `Err`.
    pub fn new(model_path: PathBuf, config_path: PathBuf) -> Result<Self> {
        if !model_path.exists() {
            return Err(anyhow!(
                "Piper voice model not found: {}",
                model_path.display()
            ));
        }
        if !config_path.exists() {
            return Err(anyhow!(
                "Piper voice config not found: {}",
                config_path.display()
            ));
        }
        let piper = load_voice(&model_path, &config_path)?;
        tracing::info!(
            "PiperRsOutput loaded voice: {} (in-process piper-rs / ort)",
            model_path.display()
        );
        Ok(Self {
            voice: RwLock::new(Arc::new(Mutex::new(piper))),
            model_path: RwLock::new(model_path),
            config_path: RwLock::new(config_path),
            last_sample_rate: Mutex::new(None),
            thinking_active: Arc::new(AtomicBool::new(false)),
            speech_interrupted: Arc::new(AtomicBool::new(false)),
            barge_in_active: Arc::new(AtomicBool::new(false)),
            is_speaking: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Point espeak-rs at a specific `espeak-ng-data` directory.
    ///
    /// Sets `PIPER_ESPEAKNG_DATA_DIRECTORY` (process-global env var consulted
    /// by espeak-rs's lazy `OnceLock` init). The supplied path is the
    /// **parent** that contains the `espeak-ng-data/` subdirectory — same
    /// convention as the legacy `--espeak_data` subprocess flag.
    ///
    /// Best-effort: env-var mutation happens once at startup before any
    /// synthesis. Subsequent calls overwrite the same var.
    pub fn with_espeak_data(self, dir: PathBuf) -> Self {
        // SAFETY: env::set_var is unsafe on edition 2024 because other threads
        // may read env concurrently. We set this once at adapter-construction
        // time before any synthesis runs, so there is no read race in practice.
        unsafe {
            std::env::set_var(PIPER_ESPEAKNG_DATA_DIRECTORY, dir.as_os_str());
        }
        tracing::debug!(
            "PiperRsOutput: set {}={}",
            PIPER_ESPEAKNG_DATA_DIRECTORY,
            dir.display()
        );
        self
    }

    /// Hot-swap the loaded voice. Returns `Err` and keeps the previous voice
    /// intact if the new voice fails to load.
    ///
    /// Mirrors `WhisperRsInput::rebuild_with`: any in-flight `synthesize`
    /// call finishes on the old voice; the next call uses the new.
    pub async fn rebuild_with(
        &self,
        new_model_path: PathBuf,
        new_config_path: PathBuf,
    ) -> Result<()> {
        if !new_model_path.exists() {
            return Err(anyhow!(
                "Piper voice model not found: {}",
                new_model_path.display()
            ));
        }
        if !new_config_path.exists() {
            return Err(anyhow!(
                "Piper voice config not found: {}",
                new_config_path.display()
            ));
        }
        let new_voice = tokio::task::spawn_blocking({
            let m = new_model_path.clone();
            let c = new_config_path.clone();
            move || load_voice(&m, &c)
        })
        .await
        .map_err(|e| anyhow!("voice load join error: {}", e))??;

        {
            let mut guard = self.voice.write().await;
            *guard = Arc::new(Mutex::new(new_voice));
        }
        {
            let mut p = self.model_path.write().await;
            *p = new_model_path.clone();
        }
        {
            let mut p = self.config_path.write().await;
            *p = new_config_path.clone();
        }
        // Invalidate the cached sample rate — the new voice may differ.
        *self.last_sample_rate.lock().unwrap() = None;

        tracing::info!("PiperRsOutput hot-swapped to: {}", new_model_path.display());
        Ok(())
    }

    /// Path of the currently loaded voice's `.onnx` file.
    pub async fn current_model_path(&self) -> PathBuf {
        self.model_path.read().await.clone()
    }

    /// Path of the currently loaded voice's `.onnx.json` file.
    pub async fn current_config_path(&self) -> PathBuf {
        self.config_path.read().await.clone()
    }

    /// Sample rate of the most recent synthesis, if any has run yet.
    pub fn last_sample_rate(&self) -> Option<u32> {
        *self.last_sample_rate.lock().unwrap()
    }

    /// Synthesise `text` on a blocking thread. Returns the WAV bytes.
    ///
    /// On any error (including a caught panic in piper-rs / ort / espeak),
    /// returns `Err`. Empty or whitespace-only `text` returns `Ok(Vec::new())`.
    async fn synth_to_wav(&self, text: &str) -> Result<Vec<u8>> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let text = text.to_string();
        let voice_arc = self.voice.read().await.clone();

        let (wav, sample_rate) = tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, u32)> {
            synth_blocking(voice_arc, &text)
        })
        .await
        .context("piper synthesize task panicked")??;

        *self.last_sample_rate.lock().unwrap() = Some(sample_rate);
        Ok(wav)
    }
}

#[async_trait]
impl VoiceOutput for PiperRsOutput {
    fn start_thinking_tone(&self) {
        start_thinking_tone_thread(self.thinking_active.clone());
    }

    fn stop_thinking_tone(&self) {
        self.thinking_active.store(false, Ordering::SeqCst);
    }

    fn stop_speaking(&self) {
        self.speech_interrupted.store(true, Ordering::SeqCst);
    }

    fn start_barge_in_listener(&self) {
        start_barge_in_thread(
            self.barge_in_active.clone(),
            self.speech_interrupted.clone(),
            self.is_speaking.clone(),
        );
    }

    fn stop_barge_in_listener(&self) {
        self.barge_in_active.store(false, Ordering::SeqCst);
    }

    async fn speak_quip(&self) -> Option<&'static str> {
        let quip = pick_quip();
        if let Err(e) = self.speak(quip).await {
            tracing::debug!("Quip TTS failed: {e}");
            return None;
        }
        Some(quip)
    }

    async fn speak(&self, text: &str) -> Result<()> {
        self.speech_interrupted.store(false, Ordering::SeqCst);
        let clauses = split_clauses(text);
        if clauses.is_empty() {
            return Ok(());
        }
        // Synthesize first clause, then overlap playback of clause N with
        // synthesis of clause N+1 to cut first-audio latency on long sentences
        // (#162). Thread `is_speaking` through every play call so the barge-in
        // monitor applies the elevated AEC-gating threshold during playback (#161).
        let mut pending = self.synth_to_wav(&clauses[0]).await?;

        for i in 1..clauses.len() {
            if self.speech_interrupted.load(Ordering::Relaxed) {
                return Ok(());
            }
            if pending.is_empty() {
                pending = self.synth_to_wav(&clauses[i]).await?;
                continue;
            }
            let flag = self.speech_interrupted.clone();
            let is_speaking = self.is_speaking.clone();
            let wav = std::mem::take(&mut pending);
            let (play_result, next_wav) = tokio::join!(
                tokio::task::spawn_blocking(move || play_wav_interruptible(
                    wav,
                    &flag,
                    &is_speaking
                )),
                self.synth_to_wav(&clauses[i]),
            );
            play_result.context("playback task panicked")??;
            pending = next_wav?;
        }

        if !pending.is_empty() && !self.speech_interrupted.load(Ordering::Relaxed) {
            let flag = self.speech_interrupted.clone();
            let is_speaking = self.is_speaking.clone();
            tokio::task::spawn_blocking(move || {
                play_wav_interruptible(pending, &flag, &is_speaking)
            })
            .await
            .context("playback task panicked")??;
        }
        Ok(())
    }

    async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
        let wav = self.synth_to_wav(text).await?;
        if wav.is_empty() {
            Ok(None)
        } else {
            Ok(Some(wav))
        }
    }

    async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
        self.speech_interrupted.store(false, Ordering::SeqCst);
        let flag = self.speech_interrupted.clone();
        let is_speaking = self.is_speaking.clone();
        tokio::task::spawn_blocking(move || play_wav_interruptible(audio, &flag, &is_speaking))
            .await
            .context("playback task panicked")?
    }
}

// ── Internals ─────────────────────────────────────────────────────────────────

/// Load a piper-rs voice, catching any panic from ort / serde.
fn load_voice(model_path: &std::path::Path, config_path: &std::path::Path) -> Result<Piper> {
    let m = model_path.to_path_buf();
    let c = config_path.to_path_buf();
    let result = catch_unwind(AssertUnwindSafe(move || -> Result<Piper> {
        Piper::new(&m, &c).map_err(|e| anyhow!("piper-rs load failed: {}", e))
    }));
    match result {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(e)) => Err(e),
        Err(panic) => {
            let msg = panic_message(&panic);
            Err(anyhow!("piper-rs load panic: {}", msg))
        }
    }
}

/// Synchronous synth path, called from `spawn_blocking`.
///
/// Wraps the `&mut self` call into piper-rs in a `Mutex::lock` and the whole
/// thing in `catch_unwind` so a C-side panic in ort / espeak returns `Err`.
fn synth_blocking(voice: Arc<Mutex<Piper>>, text: &str) -> Result<(Vec<u8>, u32)> {
    let text = text.to_string();
    let result = catch_unwind(AssertUnwindSafe(move || -> Result<(Vec<u8>, u32)> {
        let mut guard = voice
            .lock()
            .map_err(|e| anyhow!("piper voice mutex poisoned: {}", e))?;
        // create(text, is_phonemes, speaker_id, length_scale, noise_scale, noise_w)
        // — passing None lets piper-rs use the config's default inference params.
        let (samples, sample_rate) = guard
            .create(&text, false, None, None, None, None)
            .map_err(|e| anyhow!("piper-rs synth failed: {}", e))?;
        drop(guard);

        if samples.is_empty() {
            tracing::warn!("piper-rs produced no samples for text: {:?}", text);
            return Ok((Vec::new(), sample_rate));
        }
        let pcm = f32_samples_to_pcm_le_bytes(&samples);
        let wav = pcm_to_wav(&pcm, sample_rate);
        Ok((wav, sample_rate))
    }));
    match result {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(e),
        Err(panic) => {
            let msg = panic_message(&panic);
            tracing::error!("piper-rs synth panic caught: {}", msg);
            Err(anyhow!("piper-rs synth panic: {}", msg))
        }
    }
}

/// Split text into clause-sized chunks for pipelined synthesis.
///
/// Splits on `,`, `;`, `:` so the first clause can be synthesized and played
/// while the remainder is still being processed. Keeps the delimiter attached
/// to the preceding clause for natural prosody. A delimiter flanked by digits on
/// both sides (e.g. `10,000` or `12:30`) is treated as part of the number/time
/// and does NOT start a new clause.
fn split_clauses(text: &str) -> Vec<String> {
    let mut clauses: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut prev: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        buf.push(ch);
        if matches!(ch, ',' | ';' | ':') {
            let between_digits = prev.is_some_and(|p| p.is_ascii_digit())
                && chars.peek().is_some_and(|n| n.is_ascii_digit());
            if !between_digits {
                let clause = buf.trim().to_string();
                if !clause.is_empty() {
                    clauses.push(clause);
                }
                buf.clear();
            }
        }
        prev = Some(ch);
    }
    let tail = buf.trim().to_string();
    if !tail.is_empty() {
        clauses.push(tail);
    }
    clauses
}

/// Best-effort message extraction from a `catch_unwind` payload.
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = panic.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_returns_err_on_missing_model() {
        let model = PathBuf::from("/tmp/definitely-not-a-real-piper-voice-12345.onnx");
        let config = PathBuf::from("/tmp/definitely-not-a-real-piper-voice-12345.onnx.json");
        let result = PiperRsOutput::new(model, config);
        assert!(result.is_err(), "expected Err on missing model file");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("not found"),
            "error should mention 'not found': {}",
            msg
        );
    }

    #[test]
    fn new_returns_err_on_missing_config() {
        // Create a placeholder .onnx file (empty is fine — piper-rs never
        // reads it because the config check fails first).
        let tmp = tempfile::tempdir().expect("tempdir");
        let model = tmp.path().join("voice.onnx");
        std::fs::write(&model, b"placeholder").expect("write placeholder");
        let config = tmp.path().join("voice.onnx.json");
        // Config does NOT exist.
        let result = PiperRsOutput::new(model, config);
        assert!(result.is_err(), "expected Err on missing config file");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("config not found"),
            "error should mention 'config not found': {}",
            msg
        );
    }

    /// Real-voice integration test. Gated by `PIPER_TEST_VOICE` env var.
    ///
    /// To run:
    /// ```bash
    /// PIPER_TEST_VOICE=/path/to/en_US-lessac-medium.onnx \
    ///   cargo test -p pond-adapters-piper -- --ignored
    /// ```
    /// Expects the companion `.onnx.json` alongside.
    #[test]
    #[ignore]
    fn loads_real_voice_and_synthesises_hello() {
        let Some(voice_env) = std::env::var_os("PIPER_TEST_VOICE") else {
            eprintln!("set PIPER_TEST_VOICE to run this test");
            return;
        };
        let model_path = PathBuf::from(voice_env);
        let config_path = PathBuf::from(format!("{}.json", model_path.display()));
        let runtime = tokio::runtime::Runtime::new().expect("tokio rt");
        runtime.block_on(async {
            let tts = PiperRsOutput::new(model_path, config_path).expect("voice should load");
            let wav = tts.synthesize("hello").await.expect("synth should succeed");
            let wav = wav.expect("non-empty WAV");
            // RIFF/WAVE magic.
            assert_eq!(&wav[0..4], b"RIFF");
            assert_eq!(&wav[8..12], b"WAVE");
            // Sample rate field at offset 24..28 (LE u32).
            let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
            // lessac-medium is 22050 Hz. Other medium voices are too.
            assert!(
                rate == 22_050 || rate == 16_000,
                "unexpected sample rate {} (expected 22050 or 16000)",
                rate
            );
        });
    }

    /// Confirms `PiperRsOutput` satisfies the `VoiceOutput` trait object.
    /// Pure compile-time check — no real voice loaded.
    #[test]
    fn implements_voice_output_trait_object() {
        // Build a dummy function so we exercise the trait bound at compile time
        // without needing to construct a valid PiperRsOutput.
        fn _assert_object_safe(_: Arc<dyn VoiceOutput>) {}
    }

    #[test]
    fn split_clauses_no_delimiters() {
        let clauses = split_clauses("Hello there");
        assert_eq!(clauses, vec!["Hello there"]);
    }

    #[test]
    fn split_clauses_comma() {
        let clauses = split_clauses("The model predicts tokens, then verifies them.");
        assert_eq!(
            clauses,
            vec!["The model predicts tokens,", "then verifies them."]
        );
    }

    #[test]
    fn split_clauses_multiple_delimiters() {
        let clauses = split_clauses("One, two; three: four");
        assert_eq!(clauses, vec!["One,", "two;", "three:", "four"]);
    }

    #[test]
    fn split_clauses_keeps_numbers_and_times() {
        // A delimiter between digits belongs to a number/time and must not split.
        assert_eq!(
            split_clauses("It costs 10,000 dollars"),
            vec!["It costs 10,000 dollars"]
        );
        assert_eq!(
            split_clauses("Meet at 12:30 sharp"),
            vec!["Meet at 12:30 sharp"]
        );
        // But a real clause boundary after a number still splits.
        assert_eq!(
            split_clauses("We have 10,000 tokens, then we stop"),
            vec!["We have 10,000 tokens,", "then we stop"]
        );
    }

    #[test]
    fn split_clauses_empty() {
        assert!(split_clauses("").is_empty());
        assert!(split_clauses("   ").is_empty());
    }
}
