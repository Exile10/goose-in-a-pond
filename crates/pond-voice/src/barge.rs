//! Barge-in: deciding that the user has started talking over the assistant.
//!
//! ## Why this is not on the output port
//!
//! `VoiceOutput` used to carry `start_barge_in_listener` /
//! `stop_barge_in_listener`, which put the *microphone* on the *output* port —
//! and that is why the TTS adapter opened `default_input_device()` itself, a
//! third independent claim on a device only one thing can own. Those methods
//! are gone. Barge-in is now a property of the turn: it reads energy from
//! whatever owns the microphone, and TTS knows nothing about it.
//!
//! ## Why debounced
//!
//! A single loud window is a door, a cough, or the assistant's own voice
//! leaking back through the mic. Cutting the reply off for any of those is
//! worse than missing a real interruption, so it takes
//! [`CONSECUTIVE_WINDOWS`] windows over threshold — and one quiet window
//! resets the count, so noise has to be sustained, not merely loud.
//!
//! ## Why it latches
//!
//! Once fired, the turn is already being torn down. Reporting again would
//! risk a second teardown of a turn that no longer exists.

/// A source of recent microphone level.
///
/// `None` means there is nothing to read — the microphone is closed, or
/// disabled by the privacy setting. Deliberately not `Some(0.0)`: "silent" and
/// "not listening" must not be the same value, or a closed mic would read as
/// permanent silence and barge-in would look like it was working.
pub trait SpeechEnergy: Send + Sync {
    fn recent_rms(&self, window_ms: u64) -> Option<f32>;

    /// Whether this source can ever produce a level.
    ///
    /// Distinct from `recent_rms` returning `None`, which is a momentary
    /// answer — the microphone happens to be closed right now. This is
    /// permanent: there is no microphone behind this source and there never
    /// will be. A turn checks it once and skips starting the barge-in poll
    /// at all, rather than waking ten times a second to be told `None`.
    fn is_inert(&self) -> bool {
        false
    }
}

/// No microphone. Used by text-only paths and tests.
pub struct NoEnergy;

impl SpeechEnergy for NoEnergy {
    fn recent_rms(&self, _window_ms: u64) -> Option<f32> {
        None
    }

    fn is_inert(&self) -> bool {
        true
    }
}

/// How often the turn samples the level while speaking.
pub const POLL_MS: u64 = 100;
/// How much recent audio each sample covers.
pub const WINDOW_MS: u64 = 100;
/// Level above which a window counts as speech *while the assistant is
/// talking*. Higher than a silence gate would be, because the assistant's own
/// output bleeds into the microphone.
pub const THRESHOLD_WHILE_SPEAKING: f32 = 0.15;
/// Consecutive windows required. At [`POLL_MS`] this is ~300 ms of sustained
/// speech — long enough to reject a cough, short enough to feel immediate.
pub const CONSECUTIVE_WINDOWS: u32 = 3;

/// Debounced, latching barge-in detector.
#[derive(Debug, Clone)]
pub struct BargeIn {
    threshold: f32,
    required: u32,
    seen: u32,
    fired: bool,
}

impl BargeIn {
    pub fn new(threshold: f32, required: u32) -> Self {
        Self {
            threshold,
            required,
            seen: 0,
            fired: false,
        }
    }

    /// The configuration used while the assistant is speaking.
    pub fn while_speaking() -> Self {
        Self::new(THRESHOLD_WHILE_SPEAKING, CONSECUTIVE_WINDOWS)
    }

    /// Feed one window's level. Returns true exactly once, on the window that
    /// confirms the interruption.
    pub fn on_rms(&mut self, rms: f32) -> bool {
        if self.fired {
            return false;
        }
        if rms > self.threshold {
            self.seen += 1;
            if self.seen >= self.required {
                self.fired = true;
                return true;
            }
        } else {
            self.seen = 0;
        }
        false
    }

    /// Drop the run of loud windows without clearing the latch.
    ///
    /// Called when the level is unreadable — a closed microphone must not let
    /// a half-finished run persist and then complete against audio captured
    /// much later.
    pub fn reset(&mut self) {
        self.seen = 0;
    }

    pub fn has_fired(&self) -> bool {
        self.fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOUD: f32 = 0.5;
    const QUIET: f32 = 0.01;

    #[test]
    fn it_takes_sustained_speech_not_one_loud_window() {
        let mut b = BargeIn::while_speaking();
        assert!(
            !b.on_rms(LOUD),
            "one window is a cough, not an interruption"
        );
        assert!(!b.on_rms(LOUD));
        assert!(b.on_rms(LOUD), "the third confirms");
    }

    /// A door slam then quiet must not accumulate toward a later run.
    #[test]
    fn a_quiet_window_resets_the_run() {
        let mut b = BargeIn::while_speaking();
        b.on_rms(LOUD);
        b.on_rms(LOUD);
        assert!(!b.on_rms(QUIET), "reset");
        assert!(!b.on_rms(LOUD), "counting starts over");
        assert!(!b.on_rms(LOUD));
        assert!(b.on_rms(LOUD));
    }

    /// The turn is torn down on the first fire; a second would tear down a
    /// turn that no longer exists.
    #[test]
    fn it_fires_exactly_once() {
        let mut b = BargeIn::while_speaking();
        for _ in 0..CONSECUTIVE_WINDOWS {
            b.on_rms(LOUD);
        }
        assert!(b.has_fired());
        for _ in 0..10 {
            assert!(!b.on_rms(LOUD), "must not re-fire");
        }
    }

    #[test]
    fn the_threshold_is_exclusive() {
        let mut b = BargeIn::new(0.15, 1);
        assert!(!b.on_rms(0.15), "exactly at threshold is not over it");
        assert!(b.on_rms(0.1500001));
    }

    /// `reset` clears the run but must not un-fire a confirmed interruption.
    #[test]
    fn reset_clears_the_run_but_not_the_latch() {
        let mut b = BargeIn::while_speaking();
        b.on_rms(LOUD);
        b.reset();
        assert!(!b.on_rms(LOUD));
        assert!(!b.on_rms(LOUD));
        assert!(b.on_rms(LOUD), "needed a full run after reset");

        assert!(b.has_fired());
        b.reset();
        assert!(b.has_fired(), "reset must not resurrect a torn-down turn");
        assert!(!b.on_rms(LOUD));
    }

    #[test]
    fn a_single_window_configuration_fires_immediately() {
        let mut b = BargeIn::new(0.1, 1);
        assert!(b.on_rms(0.2));
    }

    /// Silence must never fire, however long it runs.
    #[test]
    fn silence_never_fires() {
        let mut b = BargeIn::while_speaking();
        for _ in 0..100 {
            assert!(!b.on_rms(QUIET));
        }
        assert!(!b.has_fired());
    }

    /// A closed or disabled microphone reads as `None`, not `Some(0.0)` —
    /// otherwise "not listening" would be indistinguishable from "silent" and
    /// barge-in would appear to work while being deaf.
    #[test]
    fn no_energy_reports_absence_rather_than_silence() {
        assert_eq!(NoEnergy.recent_rms(WINDOW_MS), None);
    }

    /// A turn reads this once instead of polling a source that can never
    /// answer. A real microphone must never claim to be inert, even while
    /// it happens to be closed.
    #[test]
    fn no_energy_declares_itself_permanently_inert() {
        assert!(NoEnergy.is_inert());

        struct ClosedMic;
        impl SpeechEnergy for ClosedMic {
            fn recent_rms(&self, _: u64) -> Option<f32> {
                None
            }
        }
        assert!(
            !ClosedMic.is_inert(),
            "a closed microphone is momentarily silent, not permanently absent"
        );
    }

    /// The published constants are what the turn's poll loop is built on.
    #[test]
    fn the_debounce_window_is_responsive_but_not_twitchy() {
        let ms = POLL_MS * CONSECUTIVE_WINDOWS as u64;
        assert!(
            (200..=500).contains(&ms),
            "{ms}ms to confirm: under 200 is twitchy, over 500 feels unresponsive"
        );
    }
}
