//! Piper TTS adapter for Goose In A Pond.
//!
//! Exports:
//! - `PiperRsOutput` — in-process `VoiceOutput` port (piper-rs / ort, default)
//!
//! ## Default — in-process (`PiperRsOutput`)
//!
//! Loads a Piper `.onnx` voice + `.onnx.json` config directly via the
//! piper-rs bindings (ONNX Runtime + espeak-rs phonemizer). No subprocess,
//! no per-utterance fork+exec cost.
//!
//! ## Shared infrastructure
//!
//! Both backends reuse the same backend-agnostic helpers from this module:
//! - `pcm_to_wav` — wraps int16 PCM into a minimal RIFF/WAV header
//! - `play_wav_interruptible` — rodio playback with 50 ms interrupt polling
//! - `start_thinking_tone_thread` — the soft working tone

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod in_process;
pub use in_process::PiperRsOutput;

// ── Shared helpers (backend-agnostic) ─────────────────────────────────────────

/// Sample rate of the generated working tone.
const TONE_RATE: u32 = 22_050;
/// One breath of the tone: a soft chime, then silence, then repeat.
const TONE_CYCLE_MS: u64 = 2_600;
/// How long the chime itself rings before the silence.
const TONE_CHIME_MS: u64 = 1_100;

/// Build one cycle of the working tone: a chime followed by silence.
///
/// The old tone was a bare 440 Hz sine, one second on and one second off,
/// forever. A pure tone at concert A with a hard cycle is about the most
/// fatiguing thing a speaker can produce, and it played through every wait.
///
/// This is a major sixth (E5 over G4) — a consonant interval, so the two
/// partials beat slowly rather than clashing — with the upper voice softer
/// than the lower, a gentle attack so it fades in instead of clicking, and an
/// exponential decay that lets it ring out. Then it rests. The silence is most
/// of the cycle, which is what makes it something you can sit through: it
/// reads as breathing rather than as an alarm.
fn working_tone_cycle() -> Vec<f32> {
    let chime_samples = (TONE_RATE as u64 * TONE_CHIME_MS / 1000) as usize;
    let cycle_samples = (TONE_RATE as u64 * TONE_CYCLE_MS / 1000) as usize;
    let mut out = Vec::with_capacity(cycle_samples);

    for i in 0..chime_samples {
        let t = i as f32 / TONE_RATE as f32;
        let progress = i as f32 / chime_samples as f32;

        // Fade in over the first 8% so the chime never clicks, then decay.
        let attack = (progress / 0.08).min(1.0);
        let decay = (-3.2 * progress).exp();
        let envelope = attack * decay;

        let root = (2.0 * std::f32::consts::PI * 392.00 * t).sin(); // G4
        let sixth = (2.0 * std::f32::consts::PI * 659.25 * t).sin(); // E5
        out.push((root * 0.6 + sixth * 0.4) * envelope * 0.5);
    }

    out.resize(cycle_samples, 0.0);
    out
}

/// Spawn the background working-tone thread.
///
/// The `active` flag is shared with the thread and cleared by
/// `stop_thinking_tone` to break the loop. Polled every 50 ms so the tone
/// stops promptly when the first sentence of the answer is ready — a tone
/// that outlives the wait is worse than no tone.
pub(crate) fn start_thinking_tone_thread(active: Arc<AtomicBool>) {
    // If already playing, don't spawn a second thread.
    if active.swap(true, Ordering::SeqCst) {
        return;
    }
    let flag = active;
    std::thread::spawn(move || {
        use rodio::{OutputStream, Sink};

        let Ok((_stream, handle)) = OutputStream::try_default() else {
            flag.store(false, Ordering::SeqCst);
            return;
        };
        let Ok(sink) = Sink::try_new(&handle) else {
            flag.store(false, Ordering::SeqCst);
            return;
        };
        sink.set_volume(0.10);

        let cycle = working_tone_cycle();
        let polls_per_cycle = TONE_CYCLE_MS / 50;

        while flag.load(Ordering::Relaxed) {
            sink.append(rodio::buffer::SamplesBuffer::new(
                1,
                TONE_RATE,
                cycle.clone(),
            ));
            for _ in 0..polls_per_cycle {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if !flag.load(Ordering::Relaxed) {
                    sink.stop();
                    return;
                }
            }
        }
        sink.stop();
    });
}

// ── WAV encoder (shared) ──────────────────────────────────────────────────────

/// Wrap raw 16-bit mono PCM in a minimal RIFF/WAV container.
///
/// Used by both backends — the legacy subprocess emits raw int16 PCM bytes
/// directly; the in-process backend converts piper-rs's f32 samples to int16
/// first via `f32_samples_to_pcm_le_bytes`.
pub(crate) use pond_voice::dsp::encode_wav_pcm16 as pcm_to_wav;

/// Convert piper-rs's f32 mono samples (normalised to roughly [-1, 1]) to
/// signed 16-bit little-endian PCM bytes ready for `pcm_to_wav`.
///
/// Clamps to avoid wraparound when the model emits the rare out-of-range
/// sample. Mirrors the conversion used in `piper-rs/examples/wav.rs`.
pub(crate) use pond_voice::dsp::f32_to_pcm16 as f32_samples_to_pcm_le_bytes;

// ── Persistent audio output ───────────────────────────────────────────────────

/// Keeps a `rodio::OutputStream` alive on a dedicated background thread.
///
/// `rodio::OutputStream` is `!Send`, so it cannot be stored in a `Send` struct
/// directly. We park it on a named thread that sleeps until the keeper is
/// dropped, then expose the `Send + Clone` `OutputStreamHandle` for creating
/// sinks from any thread.
///
/// Reusing one `OutputStreamHandle` across all TTS calls avoids the repeated
/// CoreAudio AudioUnit open/close cycle that causes progressive audio
/// degradation after several voice turns on macOS.
/// `rodio::OutputStream` is `!Send` due to cpal's CoreAudio property-listener
/// callbacks. We move it to a dedicated keeper thread and never access it from
/// any other thread, so the transfer is safe.
#[allow(dead_code)] // kept alive for its Drop (closes the audio device); never read
struct SendableStream(rodio::OutputStream);
// SAFETY: the stream is moved into the keeper thread exactly once and lives
// there until the keeper is dropped. No other thread touches it.
unsafe impl Send for SendableStream {}

pub(crate) struct AudioKeeper {
    pub(crate) handle: rodio::OutputStreamHandle,
    stop: Arc<AtomicBool>,
}

impl AudioKeeper {
    pub(crate) fn try_new() -> Result<Self> {
        let (stream, handle) =
            rodio::OutputStream::try_default().context("audio output device unavailable")?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let sendable = SendableStream(stream);
        std::thread::Builder::new()
            .name("piper-audio-keeper".into())
            .spawn(move || {
                let _stream = sendable; // keep OutputStream alive on this thread
                while !stop_clone.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            })
            .context("audio keeper thread spawn failed")?;
        Ok(Self { handle, stop })
    }
}

impl Drop for AudioKeeper {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

// ── Audio playback (shared) ───────────────────────────────────────────────────

/// Play WAV audio with interrupt support and AEC gating.
///
/// Opens a fresh `OutputStream` on each call. Used only by the legacy
/// subprocess backend (`PiperOutput`) — the in-process backend's TTS playback
/// uses `play_wav_on_handle` with a persistent `OutputStreamHandle` instead.

/// Play WAV audio on an already-open `OutputStreamHandle` with interrupt support.
///
/// Same semantics as `play_wav_interruptible` but reuses the caller's stream
/// instead of opening a new `OutputStream`. Used by `PiperRsOutput` to avoid
/// repeated CoreAudio AudioUnit churn across voice turns.
pub(crate) fn play_wav_on_handle(
    wav: Vec<u8>,
    handle: &rodio::OutputStreamHandle,
    interrupted: &AtomicBool,
) -> Result<()> {
    use rodio::{Decoder, Sink};
    use std::io::Cursor;

    let cursor = Cursor::new(wav);
    let decoder = Decoder::new(cursor).context("Failed to decode WAV for playback")?;
    let sink = Sink::try_new(handle).context("Failed to create audio sink")?;
    sink.append(decoder);

    while !sink.empty() {
        if interrupted.load(Ordering::Relaxed) {
            sink.stop();
            tracing::debug!("TTS playback interrupted by barge-in");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
    fn f32_to_pcm_bytes_clamps_and_encodes_le() {
        // 0.0 → 0; 1.0 → i16::MAX; -2.0 clamps to -1.0 → -i16::MAX (-32767).
        // Note: -i16::MAX, not i16::MIN — multiplying -1.0 * i16::MAX gives
        // -32767, which is the symmetric counterpart of the positive peak.
        let bytes = f32_samples_to_pcm_le_bytes(&[0.0, 1.0, -2.0]);
        assert_eq!(bytes.len(), 6);
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), 0);
        assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), i16::MAX);
        assert_eq!(i16::from_le_bytes([bytes[4], bytes[5]]), -i16::MAX);
    }
}

#[cfg(test)]
mod working_tone_tests {
    use super::*;

    /// Most of the cycle is silence. That ratio is what makes the tone
    /// something a person can sit through for a thirty-second answer instead
    /// of a sound they want to escape — the tone it replaced was a bare 440 Hz
    /// sine that ran half the time, forever.
    #[test]
    fn the_tone_rests_for_most_of_its_cycle() {
        let cycle = working_tone_cycle();
        let silent = cycle.iter().filter(|s| **s == 0.0).count();
        let ratio = silent as f32 / cycle.len() as f32;
        assert!(
            ratio > 0.5,
            "only {:.0}% silence — a tone that never rests is an alarm",
            ratio * 100.0
        );
    }

    /// A waveform that starts at full amplitude clicks, and a click every few
    /// seconds is more noticeable than the tone itself.
    #[test]
    fn the_chime_fades_in_rather_than_clicking() {
        let cycle = working_tone_cycle();
        assert_eq!(cycle[0], 0.0, "must start from silence");

        let attack = (TONE_RATE as usize) / 100; // first 10 ms
        let early = cycle[..attack].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let peak = cycle.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            early < peak * 0.8,
            "amplitude jumps to {early} within 10ms of a {peak} peak"
        );
    }

    /// It rings out instead of stopping dead, and it is over well before the
    /// cycle ends, leaving real silence rather than a fade that never lands.
    #[test]
    fn the_chime_decays_and_finishes_inside_its_cycle() {
        let cycle = working_tone_cycle();
        let chime = (TONE_RATE as u64 * TONE_CHIME_MS / 1000) as usize;

        let loudest = |r: &[f32]| r.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let first_third = loudest(&cycle[..chime / 3]);
        let last_third = loudest(&cycle[chime * 2 / 3..chime]);
        assert!(
            last_third < first_third * 0.5,
            "no decay: {first_third} then {last_third}"
        );

        assert!(
            cycle[chime..].iter().all(|s| *s == 0.0),
            "the rest of the cycle must be true silence"
        );
    }

    /// Clipping would turn the chime into a buzz. The playback sink applies
    /// its own gain on top, so headroom here is not optional.
    #[test]
    fn the_tone_never_clips() {
        let peak = working_tone_cycle()
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= 1.0, "clipped at {peak}");
        assert!(peak < 0.85, "no headroom for the sink's gain: {peak}");
        assert!(peak > 0.05, "inaudible at {peak}");
    }

    /// The stop poll has to divide the cycle, or stopping the tone waits for
    /// a whole extra cycle and the chime plays over the first sentence.
    #[test]
    fn the_tone_can_be_stopped_promptly() {
        assert_eq!(
            TONE_CYCLE_MS % 50,
            0,
            "the 50ms stop poll must divide the cycle"
        );
        assert!(
            TONE_CYCLE_MS / 50 >= 4,
            "too few polls per cycle to stop responsively"
        );
    }

    /// Silence has to outlast the chime, not merely exist.
    #[test]
    fn the_chime_is_shorter_than_the_rest_that_follows_it() {
        assert!(
            TONE_CHIME_MS < TONE_CYCLE_MS - TONE_CHIME_MS,
            "{}ms of chime against {}ms of silence",
            TONE_CHIME_MS,
            TONE_CYCLE_MS - TONE_CHIME_MS
        );
    }
}
