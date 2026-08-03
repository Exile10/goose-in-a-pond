//! A capture device that plays a supplied waveform.
//!
//! Deliberately **not** `#[cfg(test)]`. The point is not to test this crate —
//! it is to make the *subscribers* in other crates testable. The two capture
//! paths, the wake-word detector and the barge-in listener each opened
//! `default_input_device()` themselves, and consequently had no tests at all:
//! there was no way to drive them without a microphone and a person.
//!
//! With the owner between them and the hardware, any of those loops can be
//! driven from a script and asserted on in CI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::owner::{CaptureDevice, MicShared};

/// Feeds 16 kHz mono f32 into the ring in real-time-sized slices, then silence.
///
/// Deterministic in content, approximate in timing — which is exactly what a
/// poll loop with a 30 ms tick depends on. Tests assert on what was captured
/// and on VAD transitions, never on exact sample counts at a wall-clock
/// instant, because that would be flaky for reasons unrelated to the code.
pub struct ScriptedCapture {
    samples: Vec<f32>,
    chunk_ms: u64,
    /// Owned by the feeder thread; set on `stop` so it exits promptly.
    stop: Arc<AtomicBool>,
}

impl ScriptedCapture {
    /// Play `samples` in `chunk_ms` slices, then silence forever.
    pub fn new(samples: Vec<f32>, chunk_ms: u64) -> Self {
        Self {
            samples,
            chunk_ms: chunk_ms.max(1),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// `speech_ms` of tone followed by `silence_ms` of quiet.
    ///
    /// The shape every VAD assertion is about: onset, sustain, then a silence
    /// run long enough to confirm end-of-utterance.
    pub fn utterance(speech_ms: u64, silence_ms: u64, chunk_ms: u64) -> Self {
        let n = |ms: u64| (16_000 * ms / 1000) as usize;
        let mut s: Vec<f32> = (0..n(speech_ms))
            .map(|i| (i as f32 * 0.05).sin() * 0.5)
            .collect();
        s.extend(std::iter::repeat(0.0).take(n(silence_ms)));
        Self::new(s, chunk_ms)
    }

    /// Silence only — for asserting that nothing triggers on room tone.
    pub fn silence(ms: u64, chunk_ms: u64) -> Self {
        Self::new(vec![0.0; (16_000 * ms / 1000) as usize], chunk_ms)
    }
}

impl CaptureDevice for ScriptedCapture {
    fn start(&mut self, shared: Arc<MicShared>) -> Result<(), String> {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();

        let samples = self.samples.clone();
        let per_chunk = ((16_000 * self.chunk_ms) / 1000).max(1) as usize;
        let period = std::time::Duration::from_millis(self.chunk_ms);

        std::thread::Builder::new()
            .name("pond-mic-scripted".into())
            .spawn(move || {
                let silence = vec![0.0f32; per_chunk];
                let mut at = 0usize;
                while !stop.load(Ordering::SeqCst) {
                    let chunk: &[f32] = if at < samples.len() {
                        let end = (at + per_chunk).min(samples.len());
                        let c = &samples[at..end];
                        at = end;
                        c
                    } else {
                        // Past the script: keep the device "live" with silence
                        // so a poll loop sees a running stream, not a stall.
                        &silence
                    };
                    shared
                        .ring
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(chunk);
                    shared.bump();
                    std::thread::sleep(period);
                }
            })
            .map_err(|e| format!("could not spawn scripted capture: {e}"))?;
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner::{MicReader, MicState};
    use crate::spawn;

    fn settle(h: &crate::MicHandle, pred: impl Fn(&MicState) -> bool) {
        assert!(
            h.wait_for(pred, std::time::Duration::from_secs(2)),
            "state never settled; stuck at {:?}",
            h.state()
        );
    }

    #[test]
    fn a_script_reaches_subscribers_as_normalised_audio() {
        let dev = ScriptedCapture::utterance(200, 200, 20);
        let (h, join) = spawn(Box::new(dev), 16_000, 5_000, true);
        h.open();
        settle(&h, |s| s.is_open());

        // Let a few chunks land.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let heard = h.shared().recent(16_000);
        assert!(!heard.is_empty(), "the script must reach the ring");
        assert!(
            h.shared().recent_rms(1_600) > 0.01,
            "speech must register as energy"
        );

        h.shutdown();
        let _ = join.join();
    }

    /// Silence must not look like speech — the energy gate depends on it.
    #[test]
    fn silence_registers_as_silence() {
        let (h, join) = spawn(
            Box::new(ScriptedCapture::silence(400, 20)),
            16_000,
            5_000,
            true,
        );
        h.open();
        settle(&h, |s| s.is_open());
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(h.shared().recent_rms(1_600) < 0.001);
        h.shutdown();
        let _ = join.join();
    }

    /// The reader must deliver only what arrived after it started — a new
    /// capture beginning with the previous turn's tail is a real failure mode.
    #[test]
    fn a_reader_sees_only_audio_captured_after_it_started() {
        let (h, join) = spawn(
            Box::new(ScriptedCapture::utterance(400, 100, 20)),
            16_000,
            5_000,
            true,
        );
        h.open();
        settle(&h, |s| s.is_open());
        std::thread::sleep(std::time::Duration::from_millis(120));

        // Start reading only now; earlier audio is already buffered.
        let mut reader = MicReader::new(h.shared().clone());
        assert!(reader.drain().is_empty(), "nothing new yet");

        std::thread::sleep(std::time::Duration::from_millis(120));
        let got = reader.drain();
        assert!(!got.is_empty(), "must deliver audio captured since start");
        assert_eq!(reader.dropped(), 0, "a keeping-up reader loses nothing");

        h.shutdown();
        let _ = join.join();
    }

    /// Draining twice must not repeat audio — a capture path that re-read the
    /// same window would transcribe the same words twice.
    #[test]
    fn draining_twice_does_not_repeat_audio() {
        let (h, join) = spawn(
            Box::new(ScriptedCapture::utterance(400, 100, 20)),
            16_000,
            5_000,
            true,
        );
        h.open();
        settle(&h, |s| s.is_open());

        let mut reader = MicReader::new(h.shared().clone());
        std::thread::sleep(std::time::Duration::from_millis(100));
        let first = reader.drain();
        let second = reader.drain();
        assert!(!first.is_empty());
        assert!(
            second.is_empty() || second.len() < first.len(),
            "the second drain must not re-deliver the first"
        );

        h.shutdown();
        let _ = join.join();
    }

    /// A reader that falls more than a window behind must REPORT the hole
    /// rather than return a shorter clip that still sounds like speech.
    #[test]
    fn a_reader_that_falls_behind_counts_what_it_lost() {
        // Ring holds 100 ms; the script delivers far more than that.
        let (h, join) = spawn(
            Box::new(ScriptedCapture::utterance(2_000, 0, 20)),
            16_000,
            100,
            true,
        );
        h.open();
        settle(&h, |s| s.is_open());

        let mut reader = MicReader::new(h.shared().clone());
        std::thread::sleep(std::time::Duration::from_millis(400));
        let _ = reader.drain();
        assert!(
            reader.dropped() > 0,
            "overrunning the window must be visible, not silent"
        );

        h.shutdown();
        let _ = join.join();
    }

    /// The bug a second subscriber would otherwise hit: `CpalCapture::start`
    /// opens with a `self.stop()`, so a naive re-apply on the second `open()`
    /// drops and rebuilds a live stream mid-turn.
    #[test]
    fn a_second_open_does_not_interrupt_a_live_stream() {
        let (h, join) = spawn(
            Box::new(ScriptedCapture::utterance(1_000, 0, 20)),
            16_000,
            5_000,
            true,
        );
        h.open();
        settle(&h, |s| s.is_open());

        let mut reader = MicReader::new(h.shared().clone());
        std::thread::sleep(std::time::Duration::from_millis(80));

        h.open(); // a second subscriber arrives
        std::thread::sleep(std::time::Duration::from_millis(80));

        assert!(h.state().is_open(), "must still be open");
        assert!(
            !reader.drain().is_empty(),
            "audio must keep flowing across a second open()"
        );

        h.shutdown();
        let _ = join.join();
    }
}
