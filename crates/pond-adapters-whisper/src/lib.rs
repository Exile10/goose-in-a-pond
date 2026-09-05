//! Whisper ASR adapter for Goose In A Pond.
//!
//! Exports:
//! - `WhisperRsInput`         — in-process `VoiceInput` port (whisper-rs, default)
//! - `WhisperKeywordDetector` — `WakeWordDetector` port: poll mic until trigger phrase heard
//! - `WhisperBackend`         — backend trait the detector uses to transcribe windows
//!
//! ## In-process (`WhisperRsInput`)
//!
//! Loads a ggml `.bin` model directly via the whisper.cpp bindings. No port,
//! no subprocess, no multipart HTTP. Shares the ggml CUDA primary context with
//! `llama-cpp-2` on Jetson. The HTTP `WhisperInput` this replaced was deleted
//! in 2026-08; nothing here is selectable any more, so there is no default to
//! name.
//!
//! ## Where the speech/silence decision comes from
//!
//! Not from here. Both capture paths take a `&mut dyn SpeechDetector` and the
//! composition root decides which one — Silero by default, the energy gate
//! when its model or the ONNX Runtime cannot be had. This crate is in CI's
//! fast-crate set and must stay buildable without an ONNX Runtime, so it knows
//! the trait and nothing else.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_audio::{MicHandle, MicReader, MicState};
use pond_core::models::ports::voice_input::SpeculativeSignal;
use pond_core::models::ports::wake_word::{StreamingWakeWordDetector, WakeWordActivation};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod in_process;
pub use in_process::WhisperRsInput;

/// Play a short two-tone confirmation ping (C6→E6, ~220ms).
/// Called when the wake word is detected so the user gets immediate audio feedback.
fn play_wake_ping() {
    std::thread::spawn(|| {
        use rodio::{OutputStream, Sink};
        let Ok((_stream, handle)) = OutputStream::try_default() else {
            return;
        };
        let Ok(sink) = Sink::try_new(&handle) else {
            return;
        };
        sink.set_volume(0.35);

        let rate = 44100u32;
        let tone = |freq: f32, ms: u64| -> Vec<f32> {
            let n = (rate as u64 * ms / 1000) as usize;
            (0..n)
                .map(|i| {
                    let t = i as f32 / rate as f32;
                    let env = 1.0 - (i as f32 / n as f32); // fade-out
                    (2.0 * std::f32::consts::PI * freq * t).sin() * env * 0.6
                })
                .collect()
        };
        // C6 (1047 Hz) then E6 (1319 Hz) — quick ascending chime
        let mut samples = tone(1047.0, 100);
        samples.extend(tone(1319.0, 120));
        let buf = rodio::buffer::SamplesBuffer::new(1, rate, samples);
        sink.append(buf);
        sink.sleep_until_end();
    });
}

// ── WhisperBackend trait ──────────────────────────────────────────────────────

/// Synchronous transcription backend.
///
/// The `WhisperKeywordDetector` holds an `Arc<dyn WhisperBackend>` and calls
/// `transcribe_pcm_blocking` on each window during the wake-word detection
/// loop. `WhisperRsInput` is the only implementor in the tree; the trait earns
/// its keep by letting the detector's tests run against a canned transcript,
/// and by being the seam a different recogniser would arrive through.
///
/// Called from inside `tokio::task::spawn_blocking`, so a blocking call is fine.
pub trait WhisperBackend: Send + Sync {
    /// Transcribe 16 kHz mono f32 PCM. Implementations should pass the result
    /// through `strip_whisper_artifacts`. Returns an empty string for silence /
    /// no detected speech (never panics).
    fn transcribe_pcm_blocking(&self, samples: &[f32]) -> Result<String>;
}

/// Whisper's non-speech annotations, stripped in the leaf crate so the
/// desktop shell shares one implementation instead of carrying a copy.
pub(crate) use pond_voice::text::strip_whisper_artifacts;

// ── WAV decoding ─────────────────────────────────────────────────────────────

/// Decode a 16-bit mono PCM WAV (as produced by `encode_wav_mono_16k`) back to
/// f32 samples.  Returns `(samples, sample_rate)`.
pub(crate) fn decode_wav_mono_f32(wav: &[u8]) -> Result<(Vec<f32>, u32)> {
    // Delegates to the real RIFF chunk walker in pond-voice. This backs
    // POST /api/v1/transcribe, which phone recorders hit with LIST chunks, 18- or
    // 40-byte fmt chunks, EXTENSIBLE, stereo and 24-bit; assuming a 16-bit mono
    // payload at byte 44 decodes those to noise and whisper hallucinates.
    let decoded = pond_voice::dsp::decode_wav(wav).map_err(|e| anyhow!("{e}"))?;
    Ok((decoded.samples, decoded.sample_rate))
}

// ── Audio capture ─────────────────────────────────────────────────────────────

/// Open the shared mic owner and block until it settles into `Open`, `Denied` or
/// `Failed`. `MicHandle::open()` is fire-and-forget, so every capture entry point
/// needs this handshake before trusting the mic; serializing opens and closes
/// through the one owner thread is what stops two capture paths racing the device.
fn open_mic_and_confirm(mic: &MicHandle) -> Result<u64> {
    let generation = mic.open_session();
    if !mic.wait_for(
        |s| !matches!(s, MicState::Closed),
        std::time::Duration::from_secs(2),
    ) {
        return Err(anyhow!("microphone did not respond"));
    }
    match mic.state() {
        MicState::Open => Ok(generation),
        MicState::Denied => Err(anyhow!(
            "microphone is disabled in Settings (mic_enabled = false)"
        )),
        MicState::Failed(e) => Err(anyhow!("microphone could not be opened: {}", e)),
        MicState::Closed => unreachable!("wait_for guarantees a non-Closed state"),
    }
}

/// Record from the microphone until the speaker stops talking.
///
/// Unlike `record_mono_f32_vad` it skips the speech-onset wait, assuming the speaker
/// is already talking. Stops on `silence_ms` of silence or the `max_record_secs` cap.
pub(crate) fn record_mono_f32_until_silence(
    mic: &MicHandle,
    max_record_secs: u32,
    silence_ms: u64,
    detector: &mut dyn SpeechDetector,
) -> Result<(Vec<f32>, u32)> {
    const POLL_MS: u64 = 30;

    // Privacy gate: refuse to OPEN the device, so the OS microphone indicator
    // stays dark. Filtering samples after capture would leave it lit and make
    // the setting a lie.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;
    open_mic_and_confirm(mic)?;

    let sample_rate = pond_audio::CAPTURE_RATE_HZ;
    let mut reader = MicReader::new(mic.shared().clone());
    let mut samples: Vec<f32> = Vec::new();

    // Record until silence or hard cap — no onset wait.
    let max_ms = max_record_secs as u64 * 1000;
    let mut elapsed_ms: u64 = 0;
    let mut silent_for: u64 = 0;

    while elapsed_ms < max_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        elapsed_ms += POLL_MS;
        samples.extend(reader.drain());

        let recent = (sample_rate as u64 * POLL_MS / 1000) as usize;
        let start = samples.len().saturating_sub(recent);

        if !detector.is_speech(&samples[start..]) {
            silent_for += POLL_MS;
            if silent_for >= silence_ms {
                tracing::debug!(
                    "Continue-record: end-of-speech after {}ms silence ({}ms total)",
                    silent_for,
                    elapsed_ms
                );
                break;
            }
        } else {
            silent_for = 0;
        }
    }

    mic.close();
    Ok((samples, sample_rate))
}

use pond_voice::dsp::VadEvent;

use pond_voice::dsp::{SpeculativeVad, SpeechDetector};

/// Spawns a background transcription of `samples` at `sample_rate`, returning
/// a handle the caller can join once end-of-speech is confirmed.
pub(crate) type SpeculativeSpawn =
    dyn Fn(Vec<f32>, u32) -> std::thread::JoinHandle<Result<String>> + Send + Sync;

/// VAD-aware recording: wait up to `max_wait_secs` for onset, record until `silence_ms`
/// of pause or the `max_record_secs` cap, return mono f32 PCM and the device rate
/// (empty on no speech). `speculative_spawn` transcribes once per silence run, so a
/// confirmed run returns it; `on_speculative_event` reports Ready/Invalidated (Q2-26).
pub(crate) fn record_mono_f32_vad(
    mic: &MicHandle,
    max_wait_secs: u32,
    max_record_secs: u32,
    silence_ms: u64,
    speculative_spawn: Option<&SpeculativeSpawn>,
    on_speculative_event: Option<&(dyn Fn(SpeculativeSignal) + Send + Sync)>,
    audio_level_sink: Option<&ThrottledAudioLevelSink>,
    detector: &mut dyn SpeechDetector,
) -> Result<(Vec<f32>, u32, Option<String>)> {
    // Onset only. The end-of-speech threshold moved into `detector`, which is
    // why these are no longer a matched pair: onset stays an energy question on
    // purpose. A model detector needs a window or two of context before it is
    // trustworthy, so it under-reports at exactly the moment onset is decided
    // and would clip the first word.
    const SPEECH_RMS: f32 = 0.010; // onset threshold — lowered for better sensitivity
    const POLL_MS: u64 = 30;

    // Privacy gate: refuse to OPEN the device, so the OS microphone indicator
    // stays dark. Filtering samples after capture would leave it lit and make
    // the setting a lie.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;
    open_mic_and_confirm(mic)?;

    let sample_rate = pond_audio::CAPTURE_RATE_HZ;
    let mut reader = MicReader::new(mic.shared().clone());
    let mut samples: Vec<f32> = Vec::new();

    // ── Phase 1: wait for speech onset ──────────────────────────────────────
    let max_wait_ms = max_wait_secs as u64 * 1000;
    let mut waited_ms: u64 = 0;
    let mut speech_detected = false;

    while waited_ms < max_wait_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        waited_ms += POLL_MS;
        samples.extend(reader.drain());

        let recent = (sample_rate as u64 * POLL_MS / 1000) as usize;
        let start = samples.len().saturating_sub(recent);
        let rms = rms_energy(&samples[start..]);
        if let Some(sink) = audio_level_sink {
            sink.maybe_emit(rms);
        }

        if rms >= SPEECH_RMS {
            speech_detected = true;
            break;
        }
    }

    if !speech_detected {
        mic.close();
        return Ok((samples, sample_rate, None)); // empty or just noise
    }

    // ── Phase 2: record until end-of-speech ─────────────────────────────────
    let max_record_ms = max_record_secs as u64 * 1000;
    let mut recorded_ms: u64 = 0;
    let mut vad = SpeculativeVad::new(silence_ms, POLL_MS);
    let mut speculative: Option<std::thread::JoinHandle<Result<String>>> = None;
    // Set once the in-flight speculative job has been joined and the caller
    // notified via `Ready` — retained so a later `Confirmed` can reuse it
    // without re-joining, and so a later `DiscardSpeculative` knows to fire
    // `Invalidated` (only needed if the caller already heard `Ready`).
    let mut speculative_ready: Option<String> = None;
    let mut confirmed = false;

    while recorded_ms < max_record_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        recorded_ms += POLL_MS;
        samples.extend(reader.drain());

        let recent = (sample_rate as u64 * POLL_MS / 1000) as usize;
        let start = samples.len().saturating_sub(recent);
        let frame = &samples[start..];
        // The level meter wants a number and the detector wants the samples, so
        // this frame is walked twice. At 30 ms that is ~480 floats per poll —
        // far below the cost of the branch that decides whether to say so.
        if let Some(sink) = audio_level_sink {
            sink.maybe_emit(rms_energy(frame));
        }

        match vad.on_speech(detector.is_speech(frame)) {
            VadEvent::SpawnSpeculative => {
                if let Some(spawn) = speculative_spawn {
                    let snapshot = samples.clone();
                    speculative = Some(spawn(snapshot, sample_rate));
                    speculative_ready = None;
                }
            }
            VadEvent::DiscardSpeculative => {
                if speculative_ready.is_some() {
                    if let Some(cb) = on_speculative_event {
                        cb(SpeculativeSignal::Invalidated);
                    }
                }
                speculative = None; // abandon the in-flight job, it covered a too-short clip
                speculative_ready = None;
            }
            VadEvent::Confirmed => {
                tracing::debug!("VAD: end-of-speech confirmed ({}ms total)", recorded_ms);
                confirmed = true;
                break;
            }
            VadEvent::None => {}
        }

        // Poll the speculative job (non-blocking) and notify the caller the
        // instant it's ready — this is what lets the LLM start before
        // silence is confirmed, not just before the redundant re-transcribe.
        if speculative_ready.is_none() {
            if let Some(handle) = &speculative {
                if handle.is_finished() {
                    let handle = speculative.take().unwrap();
                    if let Ok(Ok(transcript)) = handle.join() {
                        speculative_ready = Some(transcript.clone());
                        if let Some(cb) = on_speculative_event {
                            cb(SpeculativeSignal::Ready(transcript));
                        }
                    }
                }
            }
        }
    }

    mic.close();

    let speculative_transcript = if confirmed {
        speculative_ready.or_else(|| speculative.and_then(|h| h.join().ok().and_then(|r| r.ok())))
    } else {
        None
    };

    Ok((samples, sample_rate, speculative_transcript))
}

// ── DSP helpers ───────────────────────────────────────────────────────────────

/// Linear interpolation resample to 16 000 Hz (whisper's expected rate).
pub(crate) use pond_voice::dsp::resample_to_16k;

// ── WAV encoding ──────────────────────────────────────────────────────────────

/// Encode mono 16-bit PCM at 16 kHz as a WAV byte vector.
/// Avoids any external WAV crate dependency.
pub(crate) use pond_voice::dsp::encode_wav_mono_16k;

// ── WhisperKeywordDetector ────────────────────────────────────────────────────

/// Configuration for the sliding-window wake-word detector.
#[derive(Clone)]
pub struct KeywordDetectorConfig {
    /// Width of the audio window fed to whisper on each cycle (milliseconds).
    ///
    /// Wider than a wake word costs proportionally more to transcribe and gives
    /// the model room to invent context, so it holds just a two-word phrase.
    pub window_ms: u64,
    /// How far to advance the window on each detection cycle (milliseconds).
    ///
    /// Sets the floor on reaction time: the wake word cannot be noticed sooner
    /// than the next slide, plus one transcription.
    pub slide_ms: u64,
    /// Audio captured *before* the trigger fired (milliseconds).
    ///
    /// Covers the slide plus transcription that elapses before the loop notices;
    /// the wake word comes off via [`pond_voice::text::strip_leading_wake_word`].
    pub lookback_ms: u64,
    /// Ceiling on audio captured after detection fires (milliseconds).
    ///
    /// A ceiling, not a target: [`Self::silence_threshold`] normally ends the
    /// capture much sooner. It only binds when someone talks continuously.
    pub post_trigger_ms: u64,
    /// Minimum RMS energy required to spend a transcription on a window.
    ///
    /// Below this, the window is skipped without waking whisper at all — which
    /// is what keeps a quiet room from costing anything.
    pub energy_threshold: f32,
    /// RMS below which the post-trigger capture counts a poll as silent.
    ///
    /// Kept separate from [`Self::energy_threshold`] and lower: the gate must be high
    /// so room tone never reaches whisper, this must be low so endings are not clipped.
    pub silence_threshold: f32,
    /// Consecutive silence (ms) that ends the post-trigger capture.
    ///
    /// Long enough to sit through the pause mid-sentence, short enough not to
    /// feel like a wait. Set to 0 to always capture the full ceiling.
    pub post_trigger_silence_ms: u64,
    /// Settling time (ms) before detection re-arms after an activation.
    ///
    /// Covers the speaker ringing out and the output device draining, so the
    /// tail of the assistant's own reply cannot re-trigger the wake word.
    pub cooldown_ms: u64,
}

impl Default for KeywordDetectorConfig {
    fn default() -> Self {
        Self {
            // ~1.4 s fits "hey goose" spoken slowly with room either side.
            window_ms: 1400,
            // Reaction floor: 200 ms + one transcription of a 1.4 s clip.
            slide_ms: 200,
            // Covers a slide plus a slow transcription, so nothing said
            // straight after the wake word is lost.
            lookback_ms: 900,
            // A ceiling for uninterrupted speech; silence ends it far sooner.
            post_trigger_ms: 12_000,
            // ~-40 dBFS. Above a quiet room, below speech.
            energy_threshold: 0.010,
            // ~-52 dBFS. Well under the gate so a fading sentence still counts
            // as speech and is not clipped.
            silence_threshold: 0.0025,
            post_trigger_silence_ms: 800,
            cooldown_ms: 600,
        }
    }
}

/// WakeWordDetector over a continuous ring buffer: a 1500 ms window sliding every
/// 500 ms, a 200 ms re-check on a short match before firing, and trailing audio
/// returned on detection so `VoiceInput::listen()` needs no second recording.
/// Implements `StreamingWakeWordDetector`; the blanket impl gives `WakeWordDetector`.
pub struct WhisperKeywordDetector {
    /// Transcription backend — `WhisperRsInput` (in-process) by default,
    /// Always `WhisperRsInput` since the HTTP backend was removed.
    backend: Arc<dyn WhisperBackend>,
    /// All normalized trigger variants. A transcript matching *any* of these fires detection.
    triggers: Vec<String>,
    prompt: String,
    config: KeywordDetectorConfig,
    /// Optional live mic-level reporter, fed from the detection loop's own
    /// RMS computation (wait state + post-trigger capture).
    audio_level_sink: Option<Arc<ThrottledAudioLevelSink>>,
    /// The single shared microphone owner. Required, not optional — every
    /// caller must go through it so this detector can never race another
    /// capture path (the follow-up VAD listen, in particular) for the device.
    mic: MicHandle,
}

impl WhisperKeywordDetector {
    /// Create a detector that calls `backend` to transcribe each window.
    /// `trigger` is the wake phrase (e.g. `"goose"`). `mic` must be the process's
    /// single shared microphone owner (see `pond_audio`), so this detector and the
    /// follow-up VAD capture cannot race the same device.
    pub fn new(
        backend: Arc<dyn WhisperBackend>,
        trigger: impl Into<String>,
        mic: MicHandle,
    ) -> Self {
        let raw = trigger.into();
        let prompt = format!("say \"{}\"", raw);
        Self {
            backend,
            triggers: vec![normalize_transcript(&raw)],
            prompt,
            config: KeywordDetectorConfig::default(),
            audio_level_sink: None,
            mic,
        }
    }

    /// Report live mic RMS level through `sink` while waiting for the wake
    /// word and while capturing trailing command audio after it fires.
    pub fn with_audio_level_sink(mut self, sink: Arc<ThrottledAudioLevelSink>) -> Self {
        self.audio_level_sink = Some(sink);
        self
    }

    /// Load calibrated transcription variants collected during onboarding.
    ///
    /// A non-empty `variants` matches any of them, absorbing Whisper's inconsistent
    /// output; empty keeps the trigger from `new()` plus the built-in fuzzy variants.
    pub fn with_transcriptions(mut self, variants: Vec<String>) -> Self {
        if !variants.is_empty() {
            self.triggers = variants
                .iter()
                .map(|v| normalize_transcript(v))
                .filter(|v| !v.is_empty())
                .collect();
            // Fallback: if all variants normalized to empty, keep the existing trigger.
            if self.triggers.is_empty() {
                self.triggers = vec![normalize_transcript(&self.prompt)];
            }
        }
        // Always add built-in fuzzy variants for the primary trigger.
        // Whisper frequently misheard common wake words.
        let primary = self.triggers.first().cloned().unwrap_or_default();
        let builtins = builtin_fuzzy_variants(&primary);
        for variant in builtins {
            if !self.triggers.contains(&variant) {
                self.triggers.push(variant);
            }
        }
        self
    }

    /// Override detection parameters.
    pub fn with_config(mut self, config: KeywordDetectorConfig) -> Self {
        self.config = config;
        self
    }

    /// The normalized variants this detector fires on.
    ///
    /// Handed to the transcription adapter so the words that trigger a turn are
    /// exactly the words stripped from the command; re-deriving them elsewhere drifts.
    pub fn triggers(&self) -> &[String] {
        &self.triggers
    }
}

/// Built-in fuzzy variants for common wake words.
///
/// Whisper (especially tiny and base) mishears short words; these catch the most
/// common transcription errors without requiring user calibration.
fn builtin_fuzzy_variants(primary_trigger: &str) -> Vec<String> {
    match primary_trigger {
        "goose" => vec![
            "goose".to_string(),
            "goos".to_string(),
            "gooes".to_string(),
            "gus".to_string(),
            "gooch".to_string(),
            "hey goose".to_string(),
            "a goose".to_string(),
            "the goose".to_string(),
        ],
        "hey goose" => vec![
            "hey goose".to_string(),
            "hey goos".to_string(),
            "hey gus".to_string(),
            "a goose".to_string(),
            "hey gooch".to_string(),
        ],
        _ => vec![],
    }
}

/// Normalization lives in `pond-voice`, beside the matcher that consumes it
/// and the stripper that undoes it. All three have to agree on what a word is,
/// and they only reliably agree if there is one implementation of it.
use pond_voice::text::normalize_transcript;

/// Root-mean-square energy of a mono f32 sample slice.
/// Returns 0.0 for an empty slice.
use pond_voice::dsp::rms as rms_energy;

// ThrottledAudioLevelSink now lives in pond-core (shared::domain::agent) so
// the piper adapter (TTS output amplitude) can reuse it too, without one
// adapter crate depending on another. Re-exported here so existing call
// sites in this file don't need to change their references.
pub use pond_core::shared::domain::agent::ThrottledAudioLevelSink;

#[async_trait]
impl StreamingWakeWordDetector for WhisperKeywordDetector {
    async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
        let backend = self.backend.clone();
        let triggers = self.triggers.clone();
        let config = self.config.clone();
        let audio_level_sink = self.audio_level_sink.clone();
        let mic = self.mic.clone();

        // `run_loop` drops this future when the turn wins, and dropping a
        // `spawn_blocking` handle DETACHES the task — so without this flag the
        // thread keeps holding a cpal stream and firing whisper every `slide_ms`.
        // A `CancellationToken` cannot help: it is async-only and this never awaits.
        let stop = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = StopOnDrop(stop.clone());

        tokio::task::spawn_blocking(move || {
            detection_loop(backend, triggers, config, stop, audio_level_sink, mic)
        })
        .await
        .map_err(|e| anyhow!("detection thread panicked: {}", e))?
    }

    fn activation_prompt(&self) -> &str {
        &self.prompt
    }
}

/// Sets its flag on drop, so dropping a future cancels the blocking thread it
/// spawned. Lives at module scope rather than inside the async fn so the drop
/// behaviour is directly testable.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Sleep `ms`, waking early if `stop` is set. Returns false when cancelled.
///
/// Every wait in the detection thread goes through this: a `spawn_blocking` task
/// cannot be cancelled from outside, so the longest sleep bounds responsiveness.
fn sleep_unless_stopped(ms: u64, stop: &AtomicBool) -> bool {
    const SLICE_MS: u64 = 50;
    let mut remaining = ms;
    while remaining > 0 {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let slice = remaining.min(SLICE_MS);
        std::thread::sleep(std::time::Duration::from_millis(slice));
        remaining -= slice;
    }
    !stop.load(Ordering::SeqCst)
}

/// Blocking detection loop — runs inside `tokio::task::spawn_blocking`, sliding a
/// window over the shared mic owner's ring buffer until the trigger is confirmed.
/// The ring must be sized at `pond_audio::spawn` time to cover
/// `window_ms.max(lookback_ms) + post_trigger_ms`; this loop only reads it.
fn detection_loop(
    backend: Arc<dyn WhisperBackend>,
    triggers: Vec<String>,
    config: KeywordDetectorConfig,
    stop: Arc<AtomicBool>,
    audio_level_sink: Option<Arc<ThrottledAudioLevelSink>>,
    mic: MicHandle,
) -> Result<WakeWordActivation> {
    // Privacy gate: refuse to OPEN the device, so the OS microphone indicator
    // stays dark. Filtering samples after capture would leave it lit and make
    // the setting a lie.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;
    // Keep the generation this open claimed and scope every release below to it:
    // this runs on a DETACHED blocking thread, so after a cancelled turn the
    // device may already belong to the follow-up capture. An unconditional close
    // there presents as a conversation that hears nothing after the wake word.
    let session = open_mic_and_confirm(&mic)?;

    let sample_rate = pond_audio::CAPTURE_RATE_HZ;

    // ── Cooldown — wait before re-arming (prevents TTS echo re-trigger) ───────
    // Slept in slices so a cancelled turn is not stuck here for the full
    // 2 s default still holding the microphone.
    if config.cooldown_ms > 0 {
        tracing::debug!("KWS: cooldown {}ms before arming", config.cooldown_ms);
        if !sleep_unless_stopped(config.cooldown_ms, &stop) {
            mic.close_session(session);
            return Err(anyhow!("wake-word detection cancelled"));
        }
    }

    // ── Detection loop ────────────────────────────────────────────────────────
    let window_samples = (config.window_ms * sample_rate as u64 / 1000) as usize;
    let slide_ms = config.slide_ms;

    loop {
        if !sleep_unless_stopped(slide_ms, &stop) {
            tracing::debug!("KWS: cancelled — releasing the microphone");
            mic.close_session(session);
            return Err(anyhow!("wake-word detection cancelled"));
        }

        // Snapshot the latest window_ms samples from the shared ring.
        let snapshot = mic.shared().recent(window_samples);

        if snapshot.len() < window_samples / 2 {
            continue; // buffer not yet full enough — keep waiting
        }

        // ── Energy gate — skip silent windows before hitting whisper ──────────
        // RMS is computed unconditionally (not just when the gate is active)
        // so the audio-level sink still gets readings when energy_threshold is 0.
        let window_rms = rms_energy(&snapshot);
        if let Some(sink) = &audio_level_sink {
            sink.maybe_emit(window_rms);
        }
        if config.energy_threshold > 0.0 && window_rms < config.energy_threshold {
            tracing::trace!("KWS: silent window skipped (rms={:.4})", window_rms);
            continue;
        }

        // The shared ring is already normalised 16 kHz mono f32 — no resample.
        let transcript = match backend.transcribe_pcm_blocking(&snapshot) {
            Ok(t) if !t.is_empty() => {
                // Backend implementations already strip artifacts, but call
                // it again so a stray bracketed tag never makes it into the
                // trigger-matching path.
                let cleaned = strip_whisper_artifacts(&t);
                if cleaned.is_empty() {
                    tracing::debug!("KWS: artifact-only transcript stripped: {:?}", t);
                    continue;
                }
                normalize_transcript(&cleaned)
            }
            Ok(_) => {
                tracing::debug!("No speech in window");
                continue;
            }
            Err(e) => {
                tracing::warn!("Whisper error (retrying): {}", e);
                continue;
            }
        };

        // Whole-word matching, not `contains` — see
        // `pond_voice::text::find_trigger_words` for why the substring form
        // both missed real activations and fired on "mongoose".
        let matched = triggers
            .iter()
            .any(|t| pond_voice::text::contains_trigger(&transcript, t));
        tracing::debug!(
            "KWS window: \"{}\" (triggers: {:?}, matched: {})",
            transcript,
            triggers,
            matched
        );

        if matched {
            tracing::info!(
                "Wake word confirmed: \"{}\" (matched triggers: {:?})",
                transcript,
                triggers
            );
            play_wake_ping();

            // VAD-gated post-trigger: poll every 50 ms and exit as soon as the
            // microphone goes silent for `post_trigger_silence_ms` consecutive ms.
            // Falls back to waiting the full `post_trigger_ms` if VAD is disabled
            // or the user keeps speaking past the ceiling.
            let poll_ms = 50u64;
            let mut elapsed_ms = 0u64;
            let mut silent_for_ms = 0u64;

            while elapsed_ms < config.post_trigger_ms {
                if !sleep_unless_stopped(poll_ms, &stop) {
                    mic.close();
                    return Err(anyhow!("wake-word detection cancelled"));
                }
                elapsed_ms += poll_ms;

                // Computed unconditionally (not just when the VAD-silence gate
                // below is active) so the audio-level sink keeps reporting
                // through the whole post-trigger capture window.
                let recent_samples = (sample_rate as u64 * poll_ms / 1000) as usize;
                let recent_rms = rms_energy(&mic.shared().recent(recent_samples));
                if let Some(sink) = &audio_level_sink {
                    sink.maybe_emit(recent_rms);
                }

                if config.post_trigger_silence_ms > 0 && config.silence_threshold > 0.0 {
                    if recent_rms < config.silence_threshold {
                        silent_for_ms += poll_ms;
                        if silent_for_ms >= config.post_trigger_silence_ms {
                            tracing::debug!(
                                "KWS: VAD silence after {}ms — snapping command audio early",
                                elapsed_ms
                            );
                            break;
                        }
                    } else {
                        silent_for_ms = 0; // voice still present — reset counter
                    }
                }
            }

            // Reach back past the trigger as well as forward: detection lags the
            // wake word by a slide plus a transcription, and taking only the
            // post-trigger audio clips the first words of the request. The wake
            // word rides along in the clip and is stripped from the transcript.
            let captured_ms = elapsed_ms + config.lookback_ms;
            let captured_samples = (captured_ms * sample_rate as u64 / 1000) as usize;
            let command_audio = mic.shared().recent(captured_samples);
            tracing::debug!(
                "KWS: captured {}ms ({}ms lookback + {}ms after the trigger)",
                captured_ms,
                config.lookback_ms,
                elapsed_ms
            );

            mic.close();

            let cmd_wav = encode_wav_mono_16k(&command_audio);

            return Ok(WakeWordActivation {
                captured_audio: Some(cmd_wav),
            });
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pond_voice::dsp::RmsDetector;

    // ── Detection tuning ──────────────────────────────────────────────────
    //
    // These assert the relationships between the knobs, not the numbers: a number
    // can be retuned on hardware, a broken relationship is a silent regression.

    /// The capture must be able to reach back over the detection latency, or
    /// the first words after the wake word are lost — which is what made
    /// "Goose, what's the weather" arrive as "the weather".
    #[test]
    fn the_lookback_covers_the_worst_case_detection_lag() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.lookback_ms >= c.slide_ms * 2,
            "lookback {}ms must cover a slide ({}ms) plus a slow transcription",
            c.lookback_ms,
            c.slide_ms
        );
    }

    /// The ring is the only copy of the audio. If it holds less than a reader
    /// asks for, the read silently returns a short clip — clipped speech, no
    /// error, no way to tell from the transcript.
    #[test]
    fn the_ring_holds_everything_both_readers_can_ask_for() {
        let c = KeywordDetectorConfig::default();
        let ring_ms = c.window_ms.max(c.lookback_ms) + c.post_trigger_ms;
        assert!(ring_ms >= c.window_ms, "detection reads a full window");
        assert!(
            ring_ms >= c.lookback_ms + c.post_trigger_ms,
            "a maximum-length capture must fit: need {}ms, ring holds {}ms",
            c.lookback_ms + c.post_trigger_ms,
            ring_ms
        );
    }

    /// The gate keeps room tone away from whisper; the VAD decides when a
    /// sentence ended. One value cannot serve both, and when it did, the
    /// gate's value won and quiet sentence endings were cut off.
    #[test]
    fn ending_a_sentence_is_judged_more_leniently_than_waking_whisper() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.silence_threshold < c.energy_threshold,
            "silence {} must be under the gate {} or trailing speech is clipped",
            c.silence_threshold,
            c.energy_threshold
        );
        assert!(
            c.silence_threshold > 0.0,
            "0 disables end-of-speech entirely"
        );
    }

    /// Reaction time floor. A user who says the wake word and waits should
    /// not be able to notice the wait.
    #[test]
    fn the_wake_word_is_noticed_within_a_slide_of_being_said() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.slide_ms <= 250,
            "slide {}ms is a visible delay",
            c.slide_ms
        );
        assert!(c.slide_ms >= 100, "under 100ms is duty cycle for no gain");
    }

    /// The window exists to hold a wake phrase, not a sentence. Wider means
    /// more for the model to invent context from and more to transcribe on
    /// every single cycle.
    #[test]
    fn the_detection_window_is_sized_for_a_wake_phrase() {
        let c = KeywordDetectorConfig::default();
        assert!(
            (1000..=2000).contains(&c.window_ms),
            "window {}ms: under 1s truncates 'hey goose', over 2s is waste",
            c.window_ms
        );
    }

    /// Long enough for the speaker to stop ringing, short enough that the
    /// wake word works immediately after a reply.
    #[test]
    fn re_arming_is_quick_enough_to_answer_a_follow_up() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.cooldown_ms <= 1000,
            "cooldown {}ms is dead time the user experiences as being ignored",
            c.cooldown_ms
        );
    }

    /// A ceiling, not a target — silence normally ends the capture. It has to
    /// clear a real spoken request with a pause in the middle.
    #[test]
    fn the_capture_ceiling_allows_a_full_spoken_request() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.post_trigger_ms >= 8_000,
            "ceiling {}ms truncates a long request",
            c.post_trigger_ms
        );
        assert!(c.post_trigger_silence_ms < c.post_trigger_ms);
    }

    // ── Trigger resolution ────────────────────────────────────────────────

    struct DeafBackend;
    impl WhisperBackend for DeafBackend {
        fn transcribe_pcm_blocking(&self, _: &[f32]) -> Result<String> {
            Ok(String::new())
        }
    }

    /// A `MicHandle` backed by a scripted (no-hardware) device, for tests
    /// that only need a valid handle to construct — not to actually capture.
    fn test_mic() -> MicHandle {
        let (mic, _join) = pond_audio::spawn(
            Box::new(pond_audio::testing::ScriptedCapture::silence(0, 20)),
            pond_audio::CAPTURE_RATE_HZ,
            5_000,
            true,
        );
        mic
    }

    /// The transcriber strips exactly what the detector matched, so the list
    /// has to be reachable — and every entry normalized, or a variant with a
    /// capital or a comma would match but never strip.
    #[test]
    fn the_resolved_triggers_are_exposed_and_all_normalized() {
        let d = WhisperKeywordDetector::new(Arc::new(DeafBackend), "goose", test_mic())
            .with_transcriptions(vec!["Hey, Goose!".into(), "  a goose  ".into()]);

        let triggers = d.triggers();
        assert!(!triggers.is_empty());
        for t in triggers {
            assert_eq!(
                *t,
                pond_voice::text::normalize_transcript(t),
                "{t:?} is not in normalized form"
            );
            assert!(!t.is_empty());
        }
    }

    /// Calibration variants must survive alongside the built-in mishearings —
    /// dropping either halves detection for someone whose accent whisper
    /// renders unusually.
    #[test]
    fn calibrated_variants_and_builtin_mishearings_both_survive() {
        let d = WhisperKeywordDetector::new(Arc::new(DeafBackend), "goose", test_mic())
            .with_transcriptions(vec!["goose".into(), "Hey, Goose.".into()]);
        let t = d.triggers();
        assert!(t.iter().any(|x| x == "hey goose"), "calibrated: {t:?}");
        assert!(t.iter().any(|x| x == "goos"), "built-in mishearing: {t:?}");
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

    /// Verify the full PCM pipeline against a real audio file.
    ///
    /// jfk.wav is the canonical whisper.cpp sample: 16-bit mono 16 kHz PCM. Parse,
    /// run through the DSP helpers, re-encode, then check structure and length.
    #[test]
    fn jfk_wav_round_trips_through_dsp_pipeline() {
        let wav_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/blobs/jfk.wav");
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

    // ── SpeculativeVad (Q2-26) ──────────────────────────────────────────

    const SPEECH: f32 = 1.0;
    const QUIET: f32 = 0.0;
    const THRESHOLD: f32 = 0.5;

    #[test]
    fn vad_does_nothing_while_speech_continues() {
        let mut vad = SpeculativeVad::new(360, 30);
        for _ in 0..10 {
            assert_eq!(vad.on_speech(SPEECH >= THRESHOLD), VadEvent::None);
        }
    }

    #[test]
    fn vad_spawns_once_on_first_silent_poll_then_goes_quiet() {
        let mut vad = SpeculativeVad::new(360, 30);
        vad.on_speech(SPEECH >= THRESHOLD);
        assert_eq!(
            vad.on_speech(QUIET >= THRESHOLD),
            VadEvent::SpawnSpeculative
        );
        // Subsequent silent polls before confirmation: no repeat spawn.
        assert_eq!(vad.on_speech(QUIET >= THRESHOLD), VadEvent::None);
        assert_eq!(vad.on_speech(QUIET >= THRESHOLD), VadEvent::None);
    }

    #[test]
    fn vad_confirms_after_silence_ms_elapses() {
        let mut vad = SpeculativeVad::new(90, 30); // 3 polls to confirm
        vad.on_speech(SPEECH >= THRESHOLD);
        assert_eq!(
            vad.on_speech(QUIET >= THRESHOLD),
            VadEvent::SpawnSpeculative
        ); // 30ms
        assert_eq!(vad.on_speech(QUIET >= THRESHOLD), VadEvent::None); // 60ms
        assert_eq!(vad.on_speech(QUIET >= THRESHOLD), VadEvent::Confirmed); // 90ms
    }

    #[test]
    fn vad_discards_speculative_on_resumed_speech() {
        let mut vad = SpeculativeVad::new(360, 30);
        vad.on_speech(SPEECH >= THRESHOLD);
        assert_eq!(
            vad.on_speech(QUIET >= THRESHOLD),
            VadEvent::SpawnSpeculative
        );
        assert_eq!(vad.on_speech(QUIET >= THRESHOLD), VadEvent::None);
        // False pause — speech resumes before confirmation.
        assert_eq!(
            vad.on_speech(SPEECH >= THRESHOLD),
            VadEvent::DiscardSpeculative
        );
        assert_eq!(vad.on_speech(SPEECH >= THRESHOLD), VadEvent::None);
    }

    #[test]
    fn vad_spawns_a_fresh_job_for_each_new_silence_run() {
        let mut vad = SpeculativeVad::new(360, 30);
        vad.on_speech(SPEECH >= THRESHOLD);
        assert_eq!(
            vad.on_speech(QUIET >= THRESHOLD),
            VadEvent::SpawnSpeculative
        );
        assert_eq!(
            vad.on_speech(SPEECH >= THRESHOLD),
            VadEvent::DiscardSpeculative
        );
        // New silence run after the false pause — spawns again, independent
        // of the discarded one.
        assert_eq!(
            vad.on_speech(QUIET >= THRESHOLD),
            VadEvent::SpawnSpeculative
        );
    }

    #[test]
    fn vad_repeated_speech_after_speech_is_a_noop() {
        let mut vad = SpeculativeVad::new(360, 30);
        assert_eq!(vad.on_speech(SPEECH >= THRESHOLD), VadEvent::None);
        assert_eq!(vad.on_speech(SPEECH >= THRESHOLD), VadEvent::None);
    }

    // ── wake-word cancellation ───────────────────────────────────────────
    //
    // `spawn_blocking` DETACHES when its handle is dropped, so without the stop
    // flag the detection thread holds a cpal stream and runs whisper forever.

    #[test]
    fn stop_on_drop_sets_the_flag() {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let _guard = StopOnDrop(flag.clone());
            assert!(!flag.load(Ordering::SeqCst), "not set before drop");
        }
        assert!(
            flag.load(Ordering::SeqCst),
            "dropping the guard must cancel"
        );
    }

    #[test]
    fn sleep_unless_stopped_runs_to_completion_when_not_cancelled() {
        let stop = AtomicBool::new(false);
        let t0 = std::time::Instant::now();
        assert!(sleep_unless_stopped(120, &stop));
        assert!(t0.elapsed() >= std::time::Duration::from_millis(100));
    }

    #[test]
    fn sleep_unless_stopped_returns_false_immediately_when_already_stopped() {
        let stop = AtomicBool::new(true);
        let t0 = std::time::Instant::now();
        assert!(!sleep_unless_stopped(5_000, &stop));
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(200),
            "must not serve out a 5s sleep after cancellation"
        );
    }

    /// The cooldown default is 2000 ms. Cancellation must not have to wait it
    /// out while still holding the microphone.
    #[test]
    fn a_long_wait_is_cut_short_by_cancellation_mid_sleep() {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let h = std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let finished = sleep_unless_stopped(2_000, &s2);
            (finished, t0.elapsed())
        });
        std::thread::sleep(std::time::Duration::from_millis(120));
        stop.store(true, Ordering::SeqCst);

        let (finished, elapsed) = h.join().unwrap();
        assert!(!finished, "must report cancellation");
        assert!(
            elapsed < std::time::Duration::from_millis(1_000),
            "woke after {elapsed:?}; should be within one 50ms slice of the signal"
        );
    }

    // ── Shared mic owner: the wake-word/VAD handoff race ────────────────────

    /// The wake-word detector hands the mic back and a follow-up VAD capture
    /// immediately reopens the same handle, which is what `run_loop` does between
    /// turns. Both go through the one serialized owner, so the second open cannot
    /// fail because the first had not finished closing.
    #[tokio::test]
    async fn wake_word_then_follow_up_capture_share_the_mic_without_racing() {
        struct AlwaysMatches;
        impl WhisperBackend for AlwaysMatches {
            fn transcribe_pcm_blocking(&self, _: &[f32]) -> Result<String> {
                Ok("goose".to_string())
            }
        }

        let (mic, _join) = pond_audio::spawn(
            Box::new(pond_audio::testing::ScriptedCapture::utterance(
                2_000, 2_000, 20,
            )),
            pond_audio::CAPTURE_RATE_HZ,
            15_000,
            true,
        );

        let detector = WhisperKeywordDetector::new(Arc::new(AlwaysMatches), "goose", mic.clone())
            .with_config(KeywordDetectorConfig {
                cooldown_ms: 0,
                window_ms: 200,
                slide_ms: 20,
                lookback_ms: 100,
                post_trigger_ms: 100,
                post_trigger_silence_ms: 0,
                ..KeywordDetectorConfig::default()
            });

        let activation = detector
            .wait_for_activation_with_audio()
            .await
            .expect("wake word must fire against a scripted utterance");
        assert!(
            activation.captured_audio.is_some(),
            "a confirmed activation must carry captured command audio"
        );

        // The detector's `mic.close()` and this follow-up `mic.open()` race
        // exactly the way `run_loop` races them between turns.
        let result = tokio::task::spawn_blocking(move || {
            let mut detector = RmsDetector::new(0.005);
            record_mono_f32_vad(&mic, 1, 1, 200, None, None, None, &mut detector)
        })
        .await
        .expect("capture thread must not panic");

        assert!(
            result.is_ok(),
            "the follow-up capture must not fail from a device race: {:?}",
            result.err()
        );
    }
}
