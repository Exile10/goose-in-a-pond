//! Speaker-side audio shared by every GIAP TTS engine: one persistent output
//! device ([`AudioKeeper`]), one interruptible playback routine ([`play_wav`]),
//! and the ambient working tone ([`start_thinking_tone_thread`]). The
//! turn-generation logic in [`play_wav`] covers two real bugs; read it first.

use anyhow::{Context, Result};
use pond_core::shared::domain::agent::ThrottledAudioLevelSink;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

// ── Working tone ──────────────────────────────────────────────────────────────

/// Sample rate of the generated working tone.
const TONE_RATE: u32 = 22_050;
/// One breath of the tone: a soft chime, then silence, then repeat.
const TONE_CYCLE_MS: u64 = 2_600;
/// How long the chime itself rings before the silence.
const TONE_CHIME_MS: u64 = 1_100;

/// `thinking_for` value meaning "no tone should be playing".
///
/// Generations start at 1, so zero can never collide with a real turn.
pub const TONE_OFF: u64 = 0;

/// Build one cycle of the working tone: a chime followed by silence. The chime is
/// a major sixth (E5 over G4), a consonant interval whose partials beat slowly,
/// with the upper voice softer, a gentle attack and an exponential decay. Silence
/// is most of the cycle, which is what makes it something to sit through.
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

/// Spawn the background working-tone thread for turn `mine`. `thinking_for` names
/// the turn the tone belongs to; the thread exits once it stops being that turn,
/// polled every 50 ms. A generation rather than a bool, because two turns can be
/// alive at once and their start/stop calls would interleave into neither's state.
pub fn start_thinking_tone_thread(thinking_for: Arc<AtomicU64>, mine: u64) {
    std::thread::spawn(move || {
        use rodio::{OutputStream, Sink};

        // Clear the claim on the way out of a failed start, but only if it is
        // still ours — a newer turn may already have claimed it.
        let release = |thinking_for: &AtomicU64| {
            let _ =
                thinking_for.compare_exchange(mine, TONE_OFF, Ordering::SeqCst, Ordering::SeqCst);
        };

        let Ok((_stream, handle)) = OutputStream::try_default() else {
            release(&thinking_for);
            return;
        };
        let Ok(sink) = Sink::try_new(&handle) else {
            release(&thinking_for);
            return;
        };
        sink.set_volume(0.10);

        let cycle = working_tone_cycle();
        let polls_per_cycle = TONE_CYCLE_MS / 50;
        let ours = |thinking_for: &AtomicU64| thinking_for.load(Ordering::Relaxed) == mine;

        while ours(&thinking_for) {
            sink.append(rodio::buffer::SamplesBuffer::new(
                1,
                TONE_RATE,
                cycle.clone(),
            ));
            for _ in 0..polls_per_cycle {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if !ours(&thinking_for) {
                    sink.stop();
                    return;
                }
            }
        }
        sink.stop();
    });
}

// ── Persistent audio output ───────────────────────────────────────────────────

/// `rodio::OutputStream` is `!Send` due to cpal's CoreAudio property-listener
/// callbacks. We move it to a dedicated keeper thread and never access it from
/// any other thread, so the transfer is safe.
#[allow(dead_code)] // kept alive for its Drop (closes the audio device); never read
struct SendableStream(rodio::OutputStream);
// SAFETY: the stream is moved into the keeper thread exactly once and lives
// there until the keeper is dropped. No other thread touches it.
unsafe impl Send for SendableStream {}

/// Keeps a `rodio::OutputStream` alive on a dedicated background thread, because
/// it is `!Send` and cannot live in a `Send` struct; the `Send + Clone`
/// `OutputStreamHandle` is exposed instead. Reusing one handle avoids the macOS
/// CoreAudio AudioUnit open/close cycle that degrades audio over voice turns.
pub struct AudioKeeper {
    pub handle: rodio::OutputStreamHandle,
    stop: Arc<AtomicBool>,
}

impl AudioKeeper {
    /// Open the default output device and park it on a keeper thread.
    ///
    /// `thread_name` is only for diagnostics — pass something that names the
    /// engine so a stuck thread in a backtrace says which one.
    pub fn try_new(thread_name: &str) -> Result<Self> {
        let (stream, handle) =
            rodio::OutputStream::try_default().context("audio output device unavailable")?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let sendable = SendableStream(stream);
        std::thread::Builder::new()
            .name(thread_name.to_string())
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

// ── Playback ──────────────────────────────────────────────────────────────────

/// Per-window RMS amplitude envelope from an `encode_wav_pcm16`-produced buffer
/// (44-byte header, 16-bit LE mono PCM), one value per `window_ms`. rodio offers
/// no per-sample hook once `append()` is called, so the caller replays one value
/// per poll tick; that fixed cadence maps elapsed ticks onto playback position.
fn compute_audio_envelope(wav: &[u8], window_ms: u64) -> Vec<f32> {
    const HEADER_LEN: usize = 44;
    if wav.len() <= HEADER_LEN {
        return Vec::new();
    }
    let sample_rate = u32::from_le_bytes(wav[24..28].try_into().unwrap_or([0x56, 0x22, 0, 0]));
    let pcm = &wav[HEADER_LEN..];
    let samples_per_window = ((sample_rate as u64 * window_ms) / 1000).max(1) as usize;
    let bytes_per_window = samples_per_window * 2;

    pcm.chunks(bytes_per_window)
        .map(|chunk| {
            let mut sum_sq = 0f32;
            let mut n = 0usize;
            for pair in chunk.chunks_exact(2) {
                let s = i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0;
                sum_sq += s * s;
                n += 1;
            }
            if n > 0 {
                (sum_sq / n as f32).sqrt()
            } else {
                0.0
            }
        })
        .collect()
}

/// Play WAV audio on an already-open `OutputStreamHandle` with interrupt support.
/// Reusing the caller's stream avoids the repeated CoreAudio AudioUnit churn that
/// degrades audio across voice turns. `audio_level_sink`, if given, gets one
/// amplitude reading per poll tick from `compute_audio_envelope`.
pub fn play_wav(
    wav: Vec<u8>,
    handle: &rodio::OutputStreamHandle,
    interrupted: &AtomicBool,
    utterance: &AtomicU64,
    audio_level_sink: Option<&ThrottledAudioLevelSink>,
) -> Result<()> {
    use rodio::{Decoder, Sink};
    use std::io::Cursor;

    // Which turn this audio belongs to, fixed at the moment playback starts.
    let mine = utterance.load(Ordering::SeqCst);

    const POLL_MS: u64 = 50;
    let envelope = audio_level_sink.map(|_| compute_audio_envelope(&wav, POLL_MS));

    let cursor = Cursor::new(wav);
    let decoder = Decoder::new(cursor).context("Failed to decode WAV for playback")?;
    let sink = Sink::try_new(handle).context("Failed to create audio sink")?;
    sink.append(decoder);

    let mut tick: usize = 0;
    while !sink.empty() {
        if interrupted.load(Ordering::Relaxed) {
            sink.stop();
            tracing::debug!("TTS playback interrupted by barge-in");
            return Ok(());
        }
        // A newer turn has begun, so this audio answers a stale question. The
        // interrupt flag alone cannot cover it: `begin_utterance` CLEARS the flag
        // for the incoming turn, wiping a cancellation raised inside the 50 ms
        // poll window. A generation can only be advanced past, never cleared.
        if utterance.load(Ordering::Relaxed) != mine {
            sink.stop();
            tracing::debug!("TTS playback dropped: it belongs to a superseded turn");
            return Ok(());
        }
        if let (Some(level_sink), Some(env)) = (audio_level_sink, &envelope) {
            if let Some(&level) = env.get(tick) {
                level_sink.maybe_emit(level);
            }
        }
        tick += 1;
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_header_is_correct() {
        // 2 bytes of PCM (one 16-bit sample at 22050 Hz mono)
        let pcm = vec![0x01u8, 0x00u8];
        let wav = pond_voice::dsp::encode_wav_pcm16(&pcm, 22_050);

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
        let bytes = pond_voice::dsp::f32_to_pcm16(&[0.0, 1.0, -2.0]);
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

/// Tests for the turn-generation rules, driven directly against the atomics.
/// They need no audio device: the bug is entirely in *when* the playback and tone
/// loops decide to stop, and both decisions are pure functions of two atomics.
#[cfg(test)]
mod utterance_generation_tests {
    use super::*;

    /// Exactly the decision `play_wav_on_handle` makes on each 50 ms poll.
    fn should_keep_playing(interrupted: &AtomicBool, utterance: &AtomicU64, mine: u64) -> bool {
        !interrupted.load(Ordering::Relaxed) && utterance.load(Ordering::Relaxed) == mine
    }

    /// A speculative turn is speaking and is cancelled by setting the interrupt
    /// flag; the confirmed turn then begins and `begin_utterance` CLEARS it.
    /// Without the generation counter that resurrects the cancelled audio and the
    /// user hears both replies at once.
    #[test]
    fn cancelled_audio_stays_cancelled_when_the_next_turn_begins() {
        let interrupted = AtomicBool::new(false);
        let utterance = AtomicU64::new(1);

        // The speculative turn starts speaking under generation 1.
        let speculative = utterance.load(Ordering::SeqCst);
        assert!(should_keep_playing(&interrupted, &utterance, speculative));

        // Its transcript did not match; cancel it.
        interrupted.store(true, Ordering::SeqCst);
        assert!(!should_keep_playing(&interrupted, &utterance, speculative));

        // The confirmed turn begins: generation advances, interrupt clears.
        utterance.fetch_add(1, Ordering::SeqCst);
        interrupted.store(false, Ordering::SeqCst);

        assert!(
            !should_keep_playing(&interrupted, &utterance, speculative),
            "the superseded turn's audio must not resume when the next turn clears the flag"
        );
        let confirmed = utterance.load(Ordering::SeqCst);
        assert!(
            should_keep_playing(&interrupted, &utterance, confirmed),
            "the new turn must be free to speak"
        );
    }

    /// Barge-in still has to work within a single turn.
    #[test]
    fn the_interrupt_flag_still_stops_the_current_turn() {
        let interrupted = AtomicBool::new(false);
        let utterance = AtomicU64::new(7);
        let mine = 7;

        assert!(should_keep_playing(&interrupted, &utterance, mine));
        interrupted.store(true, Ordering::SeqCst);
        assert!(!should_keep_playing(&interrupted, &utterance, mine));
    }

    /// Several sentences of one reply share a generation, so nothing about
    /// speaking repeatedly within a turn cancels the turn.
    #[test]
    fn every_sentence_of_one_reply_plays() {
        let interrupted = AtomicBool::new(false);
        let utterance = AtomicU64::new(3);
        let mine = 3;
        for sentence in 0..5 {
            assert!(
                should_keep_playing(&interrupted, &utterance, mine),
                "sentence {sentence} was dropped mid-reply"
            );
        }
    }

    // ── the working tone ──────────────────────────────────────────────────

    /// Exactly the decision the tone thread makes on each poll.
    fn tone_is_ours(thinking_for: &AtomicU64, mine: u64) -> bool {
        thinking_for.load(Ordering::Relaxed) == mine
    }

    /// The stuck tone. An older thread, mid-poll when its turn ended, used to
    /// see the flag set true again by the NEXT turn and carry on — leaving a
    /// tone playing that no turn could stop, because the turn that owned it had
    /// already finished stopping it.
    #[test]
    fn a_superseded_tone_thread_exits_instead_of_adopting_the_next_turn() {
        let thinking_for = AtomicU64::new(TONE_OFF);

        // Turn 1 starts a tone.
        let first = 1u64;
        thinking_for.store(first, Ordering::SeqCst);
        assert!(tone_is_ours(&thinking_for, first));

        // Turn 1 stops it, and turn 2 starts its own before thread 1 polls.
        thinking_for.store(TONE_OFF, Ordering::SeqCst);
        let second = 2u64;
        thinking_for.store(second, Ordering::SeqCst);

        assert!(
            !tone_is_ours(&thinking_for, first),
            "thread 1 must exit, not adopt turn 2's tone and play alongside thread 2"
        );
        assert!(tone_is_ours(&thinking_for, second));
    }

    /// Stopping is what silence means, and it must not depend on which turn
    /// happens to call it.
    #[test]
    fn stopping_the_tone_silences_every_generation() {
        let thinking_for = AtomicU64::new(5);
        thinking_for.store(TONE_OFF, Ordering::SeqCst);
        for generation in [1u64, 5, 9] {
            assert!(!tone_is_ours(&thinking_for, generation));
        }
    }

    /// A turn asking for the tone twice must not stack a second thread: the
    /// swap returns the same generation, and `start_thinking_tone` bails.
    #[test]
    fn asking_twice_within_a_turn_starts_one_thread() {
        let thinking_for = AtomicU64::new(TONE_OFF);
        let mine = 4u64;

        assert_ne!(
            thinking_for.swap(mine, Ordering::SeqCst),
            mine,
            "first claim"
        );
        assert_eq!(
            thinking_for.swap(mine, Ordering::SeqCst),
            mine,
            "second claim must be recognised as already ours"
        );
    }

    /// Zero is reserved, so a real turn can never be mistaken for "no tone".
    #[test]
    fn no_real_turn_can_collide_with_the_off_sentinel() {
        assert_eq!(TONE_OFF, 0);
        let first_ever = AtomicU64::new(1);
        assert_ne!(
            first_ever.load(Ordering::SeqCst),
            TONE_OFF,
            "generations start at 1"
        );
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;

    #[test]
    fn envelope_is_empty_for_a_header_only_wav() {
        assert!(compute_audio_envelope(&[0u8; 44], 50).is_empty());
        assert!(compute_audio_envelope(&[], 50).is_empty());
    }

    /// The envelope is what the UI's speaking-amplitude readout is drawn from,
    /// so full-scale PCM has to read as full-scale.
    #[test]
    fn envelope_tracks_amplitude() {
        let mut wav = pond_voice::dsp::encode_wav_pcm16(&[], 24_000);
        let one_window = 24_000 * 50 / 1000;
        wav.extend((0..one_window).flat_map(|_| i16::MAX.to_le_bytes()));
        let env = compute_audio_envelope(&wav, 50);
        assert_eq!(env.len(), 1);
        assert!(
            env[0] > 0.9,
            "full-scale PCM should read near 1.0, got {}",
            env[0]
        );
    }
}
