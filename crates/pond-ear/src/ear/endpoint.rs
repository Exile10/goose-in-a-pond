//! When an utterance has ended: one rule on Silero's probabilities in place of the pond's four
//! energy gates, kept apart from the models so it can be tested on any probability stream.

use super::front::RATE;

/// Silero's own hysteresis (speech at >= 0.5, over below 0.35, as the official
/// `get_speech_timestamps`), then a quiet stretch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Endpoint {
    pub on: f32,
    pub off: f32,
    /// Quiet after the last speech that ends the utterance.
    pub end_silence_ms: u32,
    /// After a wake, how long to wait for the command to start.
    pub command_timeout_ms: u32,
    /// Longest utterance.
    pub max_ms: u32,
}

impl Default for Endpoint {
    fn default() -> Self {
        Endpoint {
            on: 0.5,
            off: 0.35,
            end_silence_ms: 800,
            command_timeout_ms: 5_000,
            max_ms: 30_000,
        }
    }
}

/// Speech this far past the wake segment without a pause is the command said in one breath
/// ("hey goose what time is it").
pub const RUN_ON_MS: usize = 300;
/// The official 30 ms speech pad after the last voiced chunk.
pub const SPEECH_PAD_MS: usize = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `start..end` (absolute samples) is the whole utterance, wake phrase included.
    Utterance { start: usize, end: usize },
    /// A wake, then no command.
    Silence,
}

/// What one voice-activity step changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// Speech turned on (`Some(true)`) or off.
    pub speech: Option<bool>,
    pub outcome: Option<Outcome>,
}

#[derive(Clone, Copy, Debug)]
struct Turn {
    start: usize,
    wake_end: usize,
    /// Voice activity has gone quiet since the wake: the phrase itself is over.
    wake_done: bool,
    heard_command: bool,
    last_voice: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Endpointer {
    pub cfg: Endpoint,
    speaking: bool,
    turn: Option<Turn>,
}

impl Endpointer {
    pub fn new(cfg: Endpoint) -> Endpointer {
        Endpointer {
            cfg,
            speaking: false,
            turn: None,
        }
    }

    pub fn reset(&mut self) {
        self.speaking = false;
        self.turn = None;
    }

    pub fn speaking(&self) -> bool {
        self.speaking
    }

    pub fn listening(&self) -> bool {
        self.turn.is_some()
    }

    /// A wake (or a second one) starts a fresh utterance from the wake segment.
    pub fn wake(&mut self, start: usize, wake_end: usize) {
        self.turn = Some(Turn {
            start,
            wake_end,
            wake_done: false,
            heard_command: false,
            last_voice: wake_end,
        });
    }

    /// Listen from `start` with no wake phrase to wait out: push-to-talk, or after a barge-in.
    pub fn listen(&mut self, start: usize) {
        self.turn = Some(Turn {
            start,
            wake_end: start,
            wake_done: true,
            heard_command: false,
            last_voice: start,
        });
    }

    pub fn cancel(&mut self) {
        self.turn = None;
    }

    /// One Silero chunk ending at absolute sample `end` with speech probability `p`.
    pub fn vad(&mut self, end: usize, p: f32) -> Step {
        let was = self.speaking;
        self.speaking = if was {
            p >= self.cfg.off
        } else {
            p >= self.cfg.on
        };
        let mut step = Step {
            speech: (self.speaking != was).then_some(self.speaking),
            outcome: None,
        };
        let Some(turn) = self.turn.as_mut() else {
            return step;
        };
        let ms = |samples: usize| (samples as u64 * 1000 / RATE as u64) as u32;
        // The command is speech after the wake phrase has finished, or speech that runs on past
        // it; a pause between the two is normal.
        if self.speaking {
            turn.last_voice = end;
            if turn.wake_done || end > turn.wake_end + RATE * RUN_ON_MS / 1000 {
                turn.heard_command = true;
            }
        } else {
            turn.wake_done = true;
        }
        let turn = *turn;
        let ep = self.cfg;
        if turn.heard_command && !self.speaking && ms(end - turn.last_voice) >= ep.end_silence_ms
            || ms(end - turn.start) >= ep.max_ms
        {
            let pad = RATE * SPEECH_PAD_MS / 1000;
            step.outcome = Some(Outcome::Utterance {
                start: turn.start,
                end: (turn.last_voice + pad).min(end),
            });
            self.turn = None;
        } else if !turn.heard_command
            && !self.speaking
            && ms(end.saturating_sub(turn.wake_end)) >= ep.command_timeout_ms
        {
            step.outcome = Some(Outcome::Silence);
            self.turn = None;
        }
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHUNK: usize = 512;

    /// Feeds `(probability, chunks)` runs from absolute sample `at`; returns each outcome with the
    /// chunk end it fired on.
    fn run(e: &mut Endpointer, at: &mut usize, runs: &[(f32, usize)]) -> Vec<(usize, Outcome)> {
        let mut out = Vec::new();
        for &(p, n) in runs {
            for _ in 0..n {
                *at += CHUNK;
                if let Some(o) = e.vad(*at, p).outcome {
                    out.push((*at, o));
                }
            }
        }
        out
    }

    #[test]
    fn a_wake_a_pause_and_a_command_end_800_ms_after_the_last_speech() {
        let mut e = Endpointer::new(Endpoint::default());
        let mut at = 16_000;
        e.wake(8_000, at);
        // The tail of the wake phrase, a pause, two seconds of command, then quiet.
        let out = run(
            &mut e,
            &mut at,
            &[(0.9, 3), (0.1, 15), (0.9, 62), (0.1, 40)],
        );
        let last_voice = 16_000 + 80 * CHUNK;
        assert_eq!(out.len(), 1, "{out:?}");
        let (fired, Outcome::Utterance { start, end }) = out[0] else {
            panic!("{out:?}")
        };
        assert_eq!(start, 8_000);
        assert_eq!(end, last_voice + 480);
        let quiet_ms = (fired - last_voice) * 1000 / 16_000;
        assert!(
            (800..800 + 32).contains(&quiet_ms),
            "fired after {quiet_ms} ms of quiet"
        );
        assert!(!e.listening());
    }

    #[test]
    fn a_pause_inside_the_wake_phrase_is_not_the_end() {
        let mut e = Endpointer::new(Endpoint::default());
        let mut at = 16_000;
        e.wake(8_000, at);
        // No command yet: a second of quiet right after the wake does not end the utterance.
        let out = run(&mut e, &mut at, &[(0.1, 31)]);
        assert!(out.is_empty() && e.listening());
    }

    #[test]
    fn hysteresis_holds_speech_between_off_and_on() {
        let mut e = Endpointer::new(Endpoint::default());
        assert_eq!(e.vad(512, 0.45).speech, None);
        assert_eq!(e.vad(1024, 0.55).speech, Some(true));
        assert_eq!(e.vad(1536, 0.36).speech, None);
        assert_eq!(e.vad(2048, 0.34).speech, Some(false));
    }

    #[test]
    fn a_wake_with_nothing_after_it_times_out_as_silence() {
        let mut e = Endpointer::new(Endpoint::default());
        let mut at = 16_000;
        e.wake(8_000, at);
        let out = run(&mut e, &mut at, &[(0.1, 200)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, Outcome::Silence);
        let waited = (out[0].0 - 16_000) * 1000 / 16_000;
        assert!((5_000..5_032).contains(&waited), "{waited} ms");
    }

    #[test]
    fn a_command_in_one_breath_counts_after_the_run_on() {
        let mut e = Endpointer::new(Endpoint::default());
        let mut at = 16_000;
        e.wake(8_000, at);
        let out = run(&mut e, &mut at, &[(0.9, 40), (0.1, 30)]);
        assert!(
            matches!(out.as_slice(), [(_, Outcome::Utterance { .. })]),
            "{out:?}"
        );
    }

    #[test]
    fn endless_speech_is_cut_at_the_longest_utterance() {
        let mut e = Endpointer::new(Endpoint {
            max_ms: 3_000,
            ..Endpoint::default()
        });
        let mut at = 0;
        e.listen(0);
        let out = run(&mut e, &mut at, &[(0.9, 200)]);
        let (fired, Outcome::Utterance { end, .. }) = out[0] else {
            panic!("{out:?}")
        };
        assert_eq!(fired * 1000 / 16_000, 3_008);
        assert_eq!(end, fired);
    }

    #[test]
    fn listening_without_a_wake_takes_the_first_speech_as_the_command() {
        let mut e = Endpointer::new(Endpoint::default());
        let mut at = 0;
        e.listen(0);
        let out = run(&mut e, &mut at, &[(0.1, 5), (0.9, 10), (0.1, 30)]);
        let [(_, Outcome::Utterance { start: 0, end })] = out.as_slice() else {
            panic!("{out:?}")
        };
        assert_eq!(*end, 15 * CHUNK + 480);
    }
}
