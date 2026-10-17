//! A streaming band-limited resampler: exact polyphase Kaiser-windowed sinc for any rational rate
//! pair. The echo canceller's reference must not carry aliases the room never played back.

const HALF_ZEROS: f64 = 16.0;
const KAISER_BETA: f64 = 8.0;
/// Below the output Nyquist, so the transition band does not fold back into the passband.
const PASSBAND: f64 = 0.94;

/// Modified Bessel function of the first kind, order 0, by its series.
fn i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-12 * sum {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Output sample `n` sits at input position `n * m / l`, so there are exactly `l` phases.
#[derive(Clone)]
pub struct Resampler {
    l: usize,
    m: usize,
    half: usize,
    taps: Vec<f32>,
    hist: Vec<f32>,
    /// Absolute input index of `hist[0]`.
    base: usize,
    n_in: usize,
    next_out: usize,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Resampler {
        let (from, to) = (from.max(1) as usize, to.max(1) as usize);
        let g = gcd(from, to);
        let (l, m) = (to / g, from / g);
        let fc = if to < from {
            PASSBAND * to as f64 / from as f64
        } else {
            1.0
        };
        let half = if l == m {
            1
        } else {
            (HALF_ZEROS / fc).ceil() as usize
        };
        let mut taps = vec![0f32; l * 2 * half];
        let norm = i0(KAISER_BETA);
        for p in 0..l {
            let frac = p as f64 / l as f64;
            let row = &mut taps[p * 2 * half..(p + 1) * 2 * half];
            for (j, t) in row.iter_mut().enumerate() {
                let d = frac + half as f64 - 1.0 - j as f64;
                let r = d / half as f64;
                if l == m {
                    *t = if d == 0.0 { 1.0 } else { 0.0 };
                    continue;
                }
                if r.abs() >= 1.0 {
                    continue;
                }
                let arg = std::f64::consts::PI * fc * d;
                let sinc = if arg.abs() < 1e-12 {
                    1.0
                } else {
                    arg.sin() / arg
                };
                *t = (fc * sinc * i0(KAISER_BETA * (1.0 - r * r).sqrt()) / norm) as f32;
            }
            // Unity gain at DC in every phase, so a constant stays exactly constant.
            let sum: f32 = row.iter().sum();
            if sum != 0.0 {
                row.iter_mut().for_each(|t| *t /= sum);
            }
        }
        Resampler {
            l,
            m,
            half,
            taps,
            hist: Vec::new(),
            base: 0,
            n_in: 0,
            next_out: 0,
        }
    }

    /// Input samples an output waits for beyond its own position.
    pub fn latency(&self) -> usize {
        self.half
    }

    pub fn bytes(&self) -> usize {
        (self.taps.len() + self.hist.capacity()) * 4
    }

    /// Append the input; every output that is now computable is appended to `out`.
    pub fn process(&mut self, x: &[f32], out: &mut Vec<f32>) {
        self.hist.extend_from_slice(x);
        self.n_in += x.len();
        let two = 2 * self.half;
        loop {
            let num = self.next_out * self.m;
            let (i0, p) = (num / self.l, num % self.l);
            if i0 + self.half >= self.n_in {
                break;
            }
            let taps = &self.taps[p * two..(p + 1) * two];
            let first = i0 as isize + 1 - self.half as isize;
            let mut acc = 0f32;
            for (j, &t) in taps.iter().enumerate() {
                let k = first + j as isize;
                if k >= self.base as isize {
                    acc += t * self.hist[k as usize - self.base];
                }
            }
            out.push(acc);
            self.next_out += 1;
        }
        let keep = (self.next_out * self.m / self.l + 1).saturating_sub(self.half);
        if keep > self.base + 4_096 {
            self.hist.drain(..keep - self.base);
            self.base = keep;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: f64, hz: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (0.5 * (2.0 * std::f64::consts::PI * hz * i as f64 / rate).sin()) as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| v as f64 * v as f64).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    #[test]
    fn a_passband_tone_comes_through_and_an_alias_does_not() {
        for from in [24_000u32, 48_000, 44_100, 22_050] {
            let mut r = Resampler::new(from, 16_000);
            let mut y = Vec::new();
            r.process(&tone(from as f64, 1_000.0, from as usize), &mut y);
            let want = tone(16_000.0, 1_000.0, y.len());
            let err: Vec<f32> = y.iter().zip(&want).map(|(a, b)| a - b).skip(400).collect();
            let snr = 20.0 * (rms(&want[400..]) / rms(&err)).log10();
            assert!(snr > 60.0, "{from} Hz: 1 kHz tone at {snr:.1} dB SNR");

            let mut r = Resampler::new(from, 16_000);
            let mut z = Vec::new();
            r.process(&tone(from as f64, 9_500.0, from as usize), &mut z);
            let leak = 20.0 * (rms(&z[400..]) / 0.3535).log10();
            assert!(
                leak < -60.0,
                "{from} Hz: a 9.5 kHz tone leaks at {leak:.1} dB"
            );
        }
    }

    #[test]
    fn chunking_does_not_change_a_sample() {
        let x = tone(24_000.0, 440.0, 9_000);
        let mut whole = Vec::new();
        Resampler::new(24_000, 16_000).process(&x, &mut whole);
        for chunk in [1usize, 7, 480, 4_097] {
            let mut r = Resampler::new(24_000, 16_000);
            let mut y = Vec::new();
            for c in x.chunks(chunk) {
                r.process(c, &mut y);
            }
            assert_eq!(y.len(), whole.len());
            assert!(
                y.iter()
                    .zip(&whole)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn the_same_rate_passes_samples_through() {
        let x = tone(16_000.0, 300.0, 1_000);
        let mut y = Vec::new();
        let mut r = Resampler::new(16_000, 16_000);
        r.process(&x, &mut y);
        assert!(y.iter().zip(&x).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert_eq!(y.len() + r.latency(), x.len());
    }
}
