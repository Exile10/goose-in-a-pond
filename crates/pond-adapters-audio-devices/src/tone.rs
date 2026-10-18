//! The sound test's sound, and how a microphone's frames are read for it: two tones at once, at
//! frequencies no voice puts together, measured per 20 ms window and never kept as audio.

use pond_core::models::domain::audio_device::{dbfs, PairOutcome};

/// Not a musical interval on purpose: a voice's harmonics land on multiples of one pitch, and no
/// voice's pitch divides both of these.
pub const FREQS: [f32; 2] = [1031.0, 1523.0];
pub const TONE_MS: u64 = 350;
/// Soft: the test is heard in the room every time the pond chooses its devices.
const AMPLITUDE: f32 = 0.18;
const WINDOW_MS: u32 = 20;
/// Both tones must stand this far above what the room had at those frequencies before.
const MIN_MARGIN_DB: f32 = 12.0;
/// And make up this share of what the microphone heard, so a loud room is not mistaken for them.
const MIN_TONE_SHARE: f32 = 0.3;
/// Windows skipped at the start, where a stream opening can click.
const SETTLE_WINDOWS: usize = 2;

/// The test sound at `rate`, with a short fade at each end so it never clicks.
pub fn tone_samples(rate: u32) -> Vec<f32> {
    let n = (rate as u64 * TONE_MS / 1000) as usize;
    let attack = (rate / 50) as usize;
    let release = (rate / 12) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / rate as f32;
            let env = (i as f32 / attack as f32)
                .min(1.0)
                .min((n - i) as f32 / release as f32);
            FREQS
                .iter()
                .map(|f| (2.0 * std::f32::consts::PI * f * t).sin())
                .sum::<f32>()
                * AMPLITUDE
                * env
        })
        .collect()
}

#[derive(Debug, Clone, Copy, Default)]
struct Window {
    /// Power at each test frequency, in the same units as `energy`.
    tone: [f32; 2],
    /// Mean square of the window.
    energy: f32,
}

/// Goertzel power at `freq` over `x`, scaled so a pure sine there reads as its mean square.
fn goertzel(x: &[f32], freq: f32, rate: f32) -> f32 {
    let coeff = 2.0 * (2.0 * std::f32::consts::PI * freq / rate).cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &v in x {
        let s0 = v + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    2.0 * power / (x.len() as f32 * x.len() as f32)
}

/// Folds a microphone's frames into per-window levels as they arrive, keeping no samples.
pub struct Listener {
    rate: u32,
    window: usize,
    pending: Vec<f32>,
    windows: Vec<Window>,
}

impl Listener {
    pub fn new(rate: u32) -> Self {
        let window = (rate * WINDOW_MS / 1000).max(1) as usize;
        Self {
            rate,
            window,
            pending: Vec::with_capacity(window),
            windows: Vec::new(),
        }
    }

    pub fn push(&mut self, mono: impl IntoIterator<Item = f32>) {
        for v in mono {
            self.pending.push(v);
            if self.pending.len() == self.window {
                let energy = self.pending.iter().map(|v| v * v).sum::<f32>() / self.window as f32;
                let tone = FREQS.map(|f| goertzel(&self.pending, f, self.rate as f32));
                self.windows.push(Window { tone, energy });
                self.pending.clear();
            }
        }
    }

    /// Windows measured so far: mark where the test sound starts with this.
    pub fn windows(&self) -> usize {
        self.windows.len()
    }

    /// Whether the test sound, started at window `tone_from`, was heard above the room before it.
    pub fn verdict(&self, tone_from: usize) -> PairOutcome {
        let room: Vec<Window> = self
            .windows
            .iter()
            .take(tone_from)
            .skip(SETTLE_WINDOWS.min(tone_from.saturating_sub(1)))
            .copied()
            .collect();
        let after = self.windows.get(tone_from..).unwrap_or(&[]);
        if room.is_empty() || after.is_empty() {
            return PairOutcome::Failed {
                reason: "the microphone delivered too little to judge".to_string(),
            };
        }
        let mean = |f: &dyn Fn(&Window) -> f32| room.iter().map(f).sum::<f32>() / room.len() as f32;
        let room_energy = mean(&|w| w.energy);
        let room_dbfs = dbfs(room_energy.sqrt());
        let Some(loudest) = after
            .iter()
            .max_by(|a, b| (a.tone[0] + a.tone[1]).total_cmp(&(b.tone[0] + b.tone[1])))
        else {
            return PairOutcome::NotHeard;
        };
        if loudest.energy <= 0.0 {
            return PairOutcome::NotHeard;
        }
        let share = (loudest.tone[0] + loudest.tone[1]) / loudest.energy;
        let margins: Vec<f32> = (0..2)
            .map(|i| {
                let before = mean(&|w| w.tone[i]).max(1e-12);
                10.0 * (loudest.tone[i].max(1e-12) / before).log10()
            })
            .collect();
        let margin_db = margins.iter().copied().fold(f32::INFINITY, f32::min);
        if margin_db >= MIN_MARGIN_DB && share >= MIN_TONE_SHARE {
            PairOutcome::Heard {
                margin_db,
                room_dbfs,
            }
        } else {
            PairOutcome::NotHeard
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    /// Deterministic noise, so a test never passes or fails by chance.
    fn noise(n: usize, level: f32, seed: u32) -> Vec<f32> {
        let mut x = seed.max(1);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 * 2.0 - 1.0) * level
            })
            .collect()
    }

    fn hear(room: Vec<f32>, during: Vec<f32>) -> PairOutcome {
        let mut l = Listener::new(RATE);
        l.push(room);
        let from = l.windows();
        l.push(during);
        l.verdict(from)
    }

    fn quarter_second() -> usize {
        RATE as usize / 4
    }

    #[test]
    fn the_tone_is_short_soft_and_never_clips() {
        let t = tone_samples(48_000);
        assert!(t.len() < 48_000 / 2);
        let peak = t.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.1 && peak <= 2.0 * AMPLITUDE, "peak {peak}");
        assert!(t[0].abs() < 1e-3, "starts from silence");
    }

    #[test]
    fn the_test_sound_over_a_quiet_room_is_heard() {
        let mut during: Vec<f32> = tone_samples(RATE).iter().map(|v| v * 0.3).collect();
        let room = noise(during.len(), 0.002, 7);
        for (d, r) in during.iter_mut().zip(&room) {
            *d += r;
        }
        match hear(noise(quarter_second(), 0.002, 3), during) {
            PairOutcome::Heard {
                margin_db,
                room_dbfs,
            } => {
                assert!(margin_db > 20.0, "{margin_db}");
                assert!(room_dbfs < -50.0, "{room_dbfs}");
            }
            other => panic!("a clear test sound was missed: {other:?}"),
        }
    }

    #[test]
    fn a_speaker_that_makes_no_sound_is_not_heard() {
        let room = noise(quarter_second(), 0.002, 3);
        assert_eq!(
            hear(room, noise(quarter_second(), 0.002, 9)),
            PairOutcome::NotHeard
        );
    }

    #[test]
    fn a_muted_microphone_hears_nothing() {
        let zeros = vec![0.0; quarter_second()];
        assert_eq!(hear(zeros.clone(), zeros), PairOutcome::NotHeard);
    }

    /// Someone talking during the test is loud, but not at both frequencies at once.
    #[test]
    fn a_voice_in_the_room_is_not_taken_for_the_test_sound() {
        let rate = RATE as f32;
        let voice: Vec<f32> = (0..quarter_second())
            .map(|i| {
                let t = i as f32 / rate;
                (1..12)
                    .map(|h| (2.0 * std::f32::consts::PI * 220.0 * h as f32 * t).sin() / h as f32)
                    .sum::<f32>()
                    * 0.2
            })
            .collect();
        assert_eq!(
            hear(noise(quarter_second(), 0.002, 3), voice),
            PairOutcome::NotHeard
        );
    }

    #[test]
    fn too_little_audio_is_a_failure_to_judge_not_a_verdict() {
        let mut l = Listener::new(RATE);
        assert!(matches!(l.verdict(0), PairOutcome::Failed { .. }));
        l.push(noise(quarter_second(), 0.01, 1));
        let from = l.windows();
        assert!(matches!(l.verdict(from), PairOutcome::Failed { .. }));
    }
}
