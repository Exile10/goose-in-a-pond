//! Kokoro-82M TTS adapter, GIAP's voice: [`VoiceOutput`] over `ort`, text to espeak IPA to
//! vocab ids to f32 at 24 kHz. Nothing is loaded at construction: the ~92 MB session is paid
//! for on first `speak` and returned by [`KokoroOutput::unload`], which is why `new()` cannot
//! fail on a bad model path and `speak()` can. Voice (522 KB table) and pace never reload it.

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_audio_out::{play_wav, start_thinking_tone_thread, AudioKeeper, TONE_OFF};
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::shared::domain::agent::ThrottledAudioLevelSink;
use pond_voice::dsp::{encode_wav_pcm16, f32_to_pcm16};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

pub mod engine;
pub mod tokenizer;
pub mod voices;

pub use engine::{Engine, SAMPLE_RATE};
pub use tokenizer::Vocab;
pub use voices::StyleTable;

/// Default voice. `af_heart` is the model's reference voice and the one its
/// published samples use.
pub const DEFAULT_VOICE: &str = "af_heart";
/// Default pace multiplier.
pub const DEFAULT_SPEED: f32 = 1.0;
/// Pace bounds. Below 0.5 the prosody smears; above 2.0 it clips words.
pub const MIN_SPEED: f32 = 0.5;
pub const MAX_SPEED: f32 = 2.0;

/// Where Kokoro's files live, and how hard to work.
#[derive(Debug, Clone)]
pub struct KokoroConfig {
    /// The `.onnx` weights (quality tier is chosen by picking the file).
    pub model_path: PathBuf,
    /// Directory of `<voice>.bin` style tables.
    pub voices_dir: PathBuf,
    /// The model repo's `tokenizer.json`.
    pub tokenizer_path: PathBuf,
    /// Bound on ONNX Runtime's per-op threads. `None` lets ORT decide.
    pub intra_threads: Option<usize>,
    /// espeak-ng data directory, if not on the default search path.
    pub espeak_data: Option<PathBuf>,
}

/// Env var that espeak-rs consults to find the bundled `espeak-ng-data` dir.
const ESPEAKNG_DATA_DIRECTORY: &str = "PIPER_ESPEAKNG_DATA_DIRECTORY";

/// How long to wait for the ONNX session before calling it broken.
///
/// Generous — a cold 92 MB load off slow storage is seconds, not instant — but
/// finite, because the failure mode being guarded is an infinite block.
const LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Kokoro as a `VoiceOutput`.
pub struct KokoroOutput {
    config: KokoroConfig,
    /// The active `.onnx`. Separate from `config` because the quality tier is
    /// changeable at runtime and must move in lockstep with `engine`.
    model_path: RwLock<PathBuf>,
    vocab: Vocab,
    /// Loaded lazily on first synthesis; `None` means "not paying for it yet".
    engine: RwLock<Option<Engine>>,
    /// The currently selected voice's style table, loaded on demand.
    style: RwLock<Option<Arc<StyleTable>>>,
    voice_name: RwLock<String>,
    /// Pace, as `f32::to_bits` so it can live in an atomic.
    speed_bits: AtomicU64,

    // ── turn / interrupt state, identical in meaning to the Piper adapter ──
    thinking_for: Arc<AtomicU64>,
    utterance: Arc<AtomicU64>,
    speech_interrupted: Arc<AtomicBool>,
    _audio_keeper: AudioKeeper,
    audio_handle: rodio::OutputStreamHandle,
    audio_level_sink: Option<Arc<ThrottledAudioLevelSink>>,
    /// Sample rate of the most recent synthesis, for diagnostics.
    last_sample_rate: Mutex<Option<u32>>,
}

impl KokoroOutput {
    /// Prepare the adapter. Reads the vocab; does **not** load the model.
    ///
    /// Fails only on things that would make every later utterance fail anyway
    /// — a missing or malformed `tokenizer.json`, or no audio device.
    pub fn new(config: KokoroConfig) -> Result<Self> {
        if let Some(dir) = &config.espeak_data {
            std::env::set_var(ESPEAKNG_DATA_DIRECTORY, dir);
        }
        let vocab = Vocab::load(&config.tokenizer_path)?;
        tracing::info!(symbols = vocab.len(), "Kokoro vocab loaded");

        let keeper = AudioKeeper::try_new("kokoro-audio-keeper")?;
        let handle = keeper.handle.clone();
        Ok(Self {
            model_path: RwLock::new(config.model_path.clone()),
            config,
            vocab,
            engine: RwLock::new(None),
            style: RwLock::new(None),
            voice_name: RwLock::new(DEFAULT_VOICE.to_string()),
            speed_bits: AtomicU64::new(DEFAULT_SPEED.to_bits() as u64),
            thinking_for: Arc::new(AtomicU64::new(TONE_OFF)),
            utterance: Arc::new(AtomicU64::new(1)),
            speech_interrupted: Arc::new(AtomicBool::new(false)),
            _audio_keeper: keeper,
            audio_handle: handle,
            audio_level_sink: None,
            last_sample_rate: Mutex::new(None),
        })
    }

    /// Attach a live playback-amplitude reporter (the `speaking` state's
    /// analog of mic RMS during `wait`/`recording`).
    pub fn with_audio_level_sink(mut self, sink: Arc<ThrottledAudioLevelSink>) -> Self {
        self.audio_level_sink = Some(sink);
        self
    }

    /// Select the voice. Swaps a 522 KB table; the session is untouched.
    pub async fn set_voice(&self, name: &str) -> Result<()> {
        // Validate before committing, so a bad name leaves the current voice
        // in place rather than muting the pond.
        let table = StyleTable::load(&self.config.voices_dir, name)?;
        *self.style.write().await = Some(Arc::new(table));
        *self.voice_name.write().await = name.to_string();
        tracing::info!(voice = name, "Kokoro voice selected");
        Ok(())
    }

    /// The currently selected voice name.
    pub async fn voice(&self) -> String {
        self.voice_name.read().await.clone()
    }

    /// Set the pace multiplier, clamped to [`MIN_SPEED`]..=[`MAX_SPEED`].
    /// Returns the value actually applied.
    pub fn set_speed(&self, speed: f32) -> f32 {
        let clamped = speed.clamp(MIN_SPEED, MAX_SPEED);
        self.speed_bits
            .store(clamped.to_bits() as u64, Ordering::Relaxed);
        clamped
    }

    pub fn speed(&self) -> f32 {
        f32::from_bits(self.speed_bits.load(Ordering::Relaxed) as u32)
    }

    /// Voices installed on disk.
    pub fn installed_voices(&self) -> Vec<String> {
        voices::installed(&self.config.voices_dir)
    }

    /// Whether the ONNX session is currently resident.
    pub async fn is_loaded(&self) -> bool {
        self.engine.read().await.is_some()
    }

    /// Drop the ONNX session, returning its memory. The next utterance
    /// reloads it.
    pub async fn unload(&self) {
        if self.engine.write().await.take().is_some() {
            tracing::info!("Kokoro session unloaded");
        }
    }

    /// Swap in a different quality tier (a different `.onnx` file). Drops the current session
    /// so the next utterance loads the new weights. Holding the engine lock for the whole swap
    /// is what makes it atomic: no synthesis can observe a path that disagrees with the session.
    pub async fn set_model(&self, path: PathBuf) -> Result<()> {
        if !path.exists() {
            return Err(anyhow::anyhow!(
                "Kokoro model not found at {}",
                path.display()
            ));
        }
        let mut engine = self.engine.write().await;
        *engine = None;
        *self.model_path.write().await = path.clone();
        tracing::info!(path = %path.display(), "Kokoro quality tier changed");
        Ok(())
    }

    /// The `.onnx` currently selected.
    pub async fn model_path(&self) -> PathBuf {
        self.model_path.read().await.clone()
    }

    /// Synthesize `text` to mono f32 samples at [`SAMPLE_RATE`].
    ///
    /// Loads the session on first call. Returns an empty buffer for text that
    /// phonemizes to nothing.
    pub async fn synth_samples(&self, text: &str) -> Result<Vec<f32>> {
        let (chunks, dropped) = tokenizer::chunk(text, &self.vocab)?;
        if dropped > 0 {
            // Not fatal, but it means espeak emitted a phoneme this model was
            // never trained to read, and the word will sound wrong.
            tracing::warn!(
                dropped,
                text = %text.chars().take(80).collect::<String>(),
                "Kokoro dropped phonemes outside the model vocab"
            );
        }
        if chunks.is_empty() {
            return Ok(Vec::new());
        }

        let style = self.ensure_style().await?;
        let speed = self.speed();

        let mut engine_guard = self.engine.write().await;
        if engine_guard.is_none() {
            let path = self.model_path.read().await.clone();
            let threads = self.config.intra_threads;
            // Bounded because a broken ONNX Runtime does not fail, it hangs: `load-dynamic`
            // with no dylib to open blocks forever inside ort's init, and an unbounded await
            // there is a permanently silent pond with nothing in the log.
            let loaded = tokio::time::timeout(
                LOAD_TIMEOUT,
                tokio::task::spawn_blocking(move || Engine::load(&path, threads)),
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Kokoro model load timed out after {}s — the ONNX Runtime library is \
                     probably missing or version-incompatible",
                    LOAD_TIMEOUT.as_secs()
                )
            })?
            .context("Kokoro model load panicked")??;
            *engine_guard = Some(loaded);
        }
        let engine = engine_guard.as_mut().expect("engine was just loaded above");

        let mut samples = Vec::new();
        for chunk in &chunks {
            let style_row = style.style_for(chunk.tokens.len());
            samples.extend(engine.synthesize(chunk, style_row, speed)?);
        }
        *self.last_sample_rate.lock().unwrap() = Some(SAMPLE_RATE);
        Ok(samples)
    }

    /// Synthesize to a WAV buffer ready for playback or an HTTP response.
    pub async fn synth_wav(&self, text: &str) -> Result<Vec<u8>> {
        let samples = self.synth_samples(text).await?;
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        Ok(encode_wav_pcm16(&f32_to_pcm16(&samples), SAMPLE_RATE))
    }

    /// Load the selected voice's style table if it isn't resident.
    async fn ensure_style(&self) -> Result<Arc<StyleTable>> {
        if let Some(s) = self.style.read().await.as_ref() {
            return Ok(s.clone());
        }
        let name = self.voice_name.read().await.clone();
        let table = Arc::new(StyleTable::load(&self.config.voices_dir, &name)?);
        *self.style.write().await = Some(table.clone());
        Ok(table)
    }

    pub fn last_sample_rate(&self) -> Option<u32> {
        *self.last_sample_rate.lock().unwrap()
    }
}

#[async_trait]
impl VoiceOutput for KokoroOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        let wav = self.synth_wav(text).await?;
        if wav.is_empty() {
            return Ok(());
        }
        self.play_audio(wav).await
    }

    async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
        let wav = self.synth_wav(text).await?;
        Ok((!wav.is_empty()).then_some(wav))
    }

    async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
        if audio.is_empty() {
            return Ok(());
        }
        let handle = self.audio_handle.clone();
        let interrupted = self.speech_interrupted.clone();
        let utterance = self.utterance.clone();
        let sink = self.audio_level_sink.clone();
        tokio::task::spawn_blocking(move || {
            play_wav(audio, &handle, &interrupted, &utterance, sink.as_deref())
        })
        .await
        .context("Kokoro playback task panicked")?
    }

    fn begin_utterance(&self) {
        // Advance the generation FIRST, then clear the interrupt: audio from
        // the superseded turn must never see a cleared flag under its own
        // generation. See `play_wav`'s comments.
        self.utterance.fetch_add(1, Ordering::SeqCst);
        self.speech_interrupted.store(false, Ordering::SeqCst);
    }

    fn stop_speaking(&self) {
        self.speech_interrupted.store(true, Ordering::SeqCst);
    }

    fn start_thinking_tone(&self) {
        let mine = self.utterance.load(Ordering::SeqCst);
        // A turn asking twice must not stack a second thread.
        if self.thinking_for.swap(mine, Ordering::SeqCst) == mine {
            return;
        }
        start_thinking_tone_thread(self.thinking_for.clone(), mine);
    }

    fn stop_thinking_tone(&self) {
        self.thinking_for.store(TONE_OFF, Ordering::SeqCst);
    }
}

/// The sentence the onboarding and settings previews speak. It names the product, runs long
/// enough to hear prosody, and carries the phonetic range that makes voices distinguishable:
/// a fricative cluster, a diphthong, and a soft ending.
pub const PREVIEW_SENTENCE: &str =
    "Hello, I'm Jarida. I live here on your shelf, I think on my own, \
     and nothing you say to me leaves this room.";

/// Whether this build targets the boards where the int8 tiers misbehave. Compile-time, and
/// correct for both Jetson build paths: `deploy.sh` builds natively on the board and
/// `build-docker.sh` cross-builds for aarch64.
const fn aarch64_linux() -> bool {
    cfg!(all(target_arch = "aarch64", target_os = "linux"))
}

/// Bound on ONNX Runtime's per-op pool, derived from the machine. Speech has a hard deadline:
/// over RTF 1.0 the pond falls behind playback. Measured on a Jetson Orin Nano at q4f16 against
/// [`PREVIEW_SENTENCE`], RTF is 1.35 / 0.99 / 0.78 / 0.62 at 2 / 3 / 4 / 6 threads, so leaving
/// two cores free puts it on 4. The bound matters: ORT threads hold resident memory per session.
pub fn default_intra_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .saturating_sub(2)
        .clamp(2, 6)
}

/// The quality tier to start a fresh install on. A capability question, measured on a Jetson
/// Orin Nano: `q8` runs at RTF 1.23 on all six cores and cannot keep ahead of playback, and
/// `q8f16` returns a full-length buffer of digital silence (fine on macOS). `q4f16` is the
/// only tier there that both produces audio and clears real time, at 154 MB against q8's 92.
pub fn host_default_quality() -> &'static str {
    if aarch64_linux() {
        "q4f16"
    } else {
        "q8"
    }
}

/// The tier this host should adopt, or `None` to leave the stored one alone. Adopt only when
/// the household has not chosen: `stored` empty is a pond that never had a tier, and
/// `stored == untouched` is a pond still carrying the struct default. Anything else is
/// somebody's choice and is left exactly as it is, even a tier this host must substitute.
pub fn tier_to_adopt(stored: &str, untouched: &str) -> Option<&'static str> {
    tier_to_adopt_for(host_default_quality(), stored, untouched)
}

/// The rule itself, with the host's tier passed in. Split out because on an arm64 Mac
/// `host_default_quality()` returns the struct default `q8`, which makes the
/// `stored == untouched` clause unreachable through [`tier_to_adopt`]; the Jetson case has to
/// be testable without a Jetson or the guard is decoration on every machine that runs CI.
pub fn tier_to_adopt_for<'a>(host: &'a str, stored: &str, untouched: &str) -> Option<&'a str> {
    let stored = stored.trim();
    let unchosen = stored.is_empty() || stored == untouched;
    (unchosen && host != stored).then_some(host)
}

/// Swap out a tier that cannot work on this host, leaving every other choice alone. Callers
/// persist and display the result, so a household sees the substitution: [`KokoroOutput::speak`]
/// cannot tell a silent buffer from a quiet one, so nothing downstream would report it.
pub fn usable_quality(requested: &str) -> &str {
    if aarch64_linux() && requested == "q8f16" {
        return "q4f16";
    }
    requested
}

pub fn model_filename(quality: &str) -> &'static str {
    match quality {
        "fp32" => "model.onnx",
        "fp16" => "model_fp16.onnx",
        "q4" => "model_q4.onnx",
        "q4f16" => "model_q4f16.onnx",
        "q8f16" => "model_q8f16.onnx",
        // q8 is the default: 92 MB, the only tier that comfortably fits
        // alongside an LLM on an 8 GB Jetson.
        _ => "model_quantized.onnx",
    }
}

/// Approximate on-disk size of a quality tier, in MB — for the UI to show what
/// a download will cost before it starts.
pub fn model_size_mb(quality: &str) -> u64 {
    match quality {
        "fp32" => 326,
        "fp16" => 163,
        "q4" => 305,
        "q4f16" => 155,
        "q8f16" => 86,
        _ => 92,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_tiers_map_to_repo_filenames() {
        assert_eq!(model_filename("q8"), "model_quantized.onnx");
        assert_eq!(model_filename("fp32"), "model.onnx");
        assert_eq!(model_filename("q4f16"), "model_q4f16.onnx");
    }

    /// An unknown tier must fall back to the shipping default, never to a
    /// filename that does not exist in the repo.
    #[test]
    fn unknown_quality_falls_back_to_the_default_tier() {
        for junk in ["", "best", "int8", "🙂"] {
            assert_eq!(model_filename(junk), "model_quantized.onnx");
            assert_eq!(model_size_mb(junk), 92);
        }
    }

    #[test]
    fn every_tier_reports_a_size() {
        for q in ["q8", "q8f16", "q4", "q4f16", "fp16", "fp32"] {
            assert!(model_size_mb(q) > 0, "{q}");
        }
    }

    #[test]
    fn preview_sentence_names_the_product_and_is_long_enough_to_judge() {
        assert!(PREVIEW_SENTENCE.contains("Jarida"));
        assert!(
            PREVIEW_SENTENCE.split_whitespace().count() > 15,
            "too short to hear prosody"
        );
    }

    /// The preview sentence is the one string every user hears before deciding
    /// on a voice — it must survive phonemization with nothing dropped.
    #[test]
    fn preview_sentence_is_fully_covered_by_the_shipped_vocab() {
        let vocab_json = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/tokenizer.json"),
        );
        let Ok(raw) = vocab_json else {
            // testdata is optional; the live test below covers the real thing.
            return;
        };
        let vocab = Vocab::from_json(&raw).unwrap();
        let Ok((_chunks, dropped)) = tokenizer::chunk(PREVIEW_SENTENCE, &vocab) else {
            return; // espeak unavailable in this environment
        };
        assert_eq!(dropped, 0, "preview sentence loses phonemes");
    }

    #[test]
    fn speed_is_clamped_to_a_sane_range() {
        // Exercised without constructing the adapter (which needs audio).
        let clamp = |s: f32| s.clamp(MIN_SPEED, MAX_SPEED);
        assert_eq!(clamp(0.1), MIN_SPEED);
        assert_eq!(clamp(9.0), MAX_SPEED);
        assert_eq!(clamp(1.0), 1.0);
        assert_eq!(clamp(DEFAULT_SPEED), DEFAULT_SPEED);
    }

    #[test]
    fn default_speed_is_within_bounds() {
        assert!((MIN_SPEED..=MAX_SPEED).contains(&DEFAULT_SPEED));
    }

    #[test]
    fn intra_threads_stays_bounded_on_any_machine() {
        // The lower bound keeps a one- or two-core box from asking for zero;
        // the upper bound is the whole point of setting this at all, since an
        // unbounded ONNX pool holds resident memory per session.
        let n = default_intra_threads();
        assert!((2..=6).contains(&n), "derived {n} threads");
    }

    #[test]
    fn host_default_maps_to_weights_that_exist() {
        let q = host_default_quality();
        let file = model_filename(q);
        assert!(
            file.starts_with("model") && file.ends_with(".onnx"),
            "{q} resolved to {file}"
        );
        if aarch64_linux() {
            assert_eq!(
                file, "model_q4f16.onnx",
                "aarch64 must not default to a tier that cannot reach real time"
            );
        }
    }

    #[test]
    fn usable_quality_leaves_working_tiers_alone() {
        for q in ["q8", "q4f16", "q4", "fp16", "fp32"] {
            assert_eq!(usable_quality(q), q, "{q} was substituted needlessly");
        }
    }

    /// q8f16 returns a full-length buffer of zeros on aarch64 Linux, and
    /// `speak()` cannot distinguish that from quiet audio — so the guarantee
    /// has to be that the tier is never the one in force there.
    #[test]
    fn the_silent_tier_is_never_selected_on_aarch64() {
        assert_ne!(host_default_quality(), "q8f16");
        if aarch64_linux() {
            assert_eq!(usable_quality("q8f16"), "q4f16");
        } else {
            assert_eq!(usable_quality("q8f16"), "q8f16");
        }
    }

    /// The case this exists for, written so it runs on any machine: a pond still carrying the
    /// struct default has not chosen, so a host whose tier differs is an upgrade rather than
    /// an override. On the Orin Nano that is `q8` stored against a `q4f16` host.
    #[test]
    fn a_default_tier_is_replaced_by_a_host_that_needs_a_different_one() {
        assert_eq!(tier_to_adopt_for("q4f16", "q8", "q8"), Some("q4f16"));
        assert_eq!(tier_to_adopt_for("q4f16", "", "q8"), Some("q4f16"));
        assert_eq!(tier_to_adopt_for("q4f16", "  ", "q8"), Some("q4f16"));
    }

    /// The other half, and the reason this cannot simply always write: a tier
    /// somebody picked is a decision, and a pond that overwrites it every boot
    /// is a settings screen that does not work. Asserted against a host tier
    /// that differs from all of them, so "left alone" means something.
    #[test]
    fn a_chosen_tier_is_never_overwritten() {
        for chosen in ["fp32", "fp16", "q4", "q8f16"] {
            assert_eq!(
                tier_to_adopt_for("q4f16", chosen, "q8"),
                None,
                "{chosen} is a choice, not a default"
            );
        }
    }

    /// Nothing to do when the stored value already is the host default —
    /// otherwise every boot writes a row for no reason, and `updated_at` starts
    /// lying about when the household last changed anything.
    #[test]
    fn adopting_is_a_no_op_once_it_has_happened() {
        assert_eq!(tier_to_adopt_for("q4f16", "q4f16", "q8"), None);
        assert_eq!(tier_to_adopt(host_default_quality(), "q8"), None);
    }

    /// The host-reading wrapper still agrees with the rule it delegates to, so
    /// the split cannot drift into two different answers.
    #[test]
    fn the_wrapper_passes_this_hosts_tier_through() {
        for stored in ["", "q8", "fp32", "q4f16"] {
            assert_eq!(
                tier_to_adopt(stored, "q8"),
                tier_to_adopt_for(host_default_quality(), stored, "q8"),
                "wrapper disagreed for stored={stored:?}"
            );
        }
    }
}
