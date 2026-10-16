//! Synthetic signals for the integration tests: no recording of anyone is in the repository.
//! `oracle/golden.py` computes the same formulas, so its goldens match these inputs.
#![allow(dead_code)]

use std::f64::consts::PI;
use std::path::PathBuf;

pub const RATE: usize = 16_000;

/// The oracle's LCG: `(s >> 8) / 2^24`, uniform in [0, 1).
pub struct Lcg(pub u32);

impl Lcg {
    pub fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f64 / 16_777_216.0
    }

    /// Uniform in [-1, 1).
    pub fn bipolar(&mut self) -> f64 {
        2.0 * self.next() - 1.0
    }
}

fn formant(f: f64, centre: f64, width: f64) -> f64 {
    1.0 / (1.0 + ((f - centre) / width).powi(2))
}

/// A voice-like signal: harmonics of a gliding f0 under three formants, syllables at about 3 Hz,
/// fricative noise between them and a faint noise floor. Quantised to int16 like a capture.
pub fn voice(n: usize, seed: u32, f0: f64, formants: [f64; 3]) -> Vec<f32> {
    let mut rng = Lcg(seed);
    let (glide, depth) = (2.3, 0.15);
    (0..n)
        .map(|i| {
            let t = i as f64 / RATE as f64;
            let f = f0 * (1.0 + depth * (2.0 * PI * glide * t).sin());
            let phase = 2.0
                * PI
                * f0
                * (t + depth / (2.0 * PI * glide) * (1.0 - (2.0 * PI * glide * t).cos()));
            let mut voiced = 0.0;
            let mut h = 1.0;
            while h * f0 * 1.15 < 7_000.0 {
                let fr = h * f;
                let a = formant(fr, formants[0], 130.0)
                    + 0.7 * formant(fr, formants[1], 170.0)
                    + 0.4 * formant(fr, formants[2], 250.0);
                voiced += a * (h * phase).sin();
                h += 1.0;
            }
            let syl = (2.0 * PI * 3.1 * t).sin();
            let g = syl.max(0.0).powf(0.6);
            let noise = rng.bipolar();
            let fricative = if syl < -0.6 { 0.05 * noise } else { 0.0 };
            let x = 0.08 * voiced * g + fricative + 0.002 * rng.bipolar();
            ((x * 32767.0 + 0.5).floor().clamp(-32768.0, 32767.0) / 32768.0) as f32
        })
        .collect()
}

/// Uniform noise, quantised like `voice`.
pub fn noise(n: usize, seed: u32, amp: f64) -> Vec<f32> {
    let mut rng = Lcg(seed);
    (0..n)
        .map(|_| {
            ((amp * rng.bipolar() * 32767.0 + 0.5)
                .floor()
                .clamp(-32768.0, 32767.0)
                / 32768.0) as f32
        })
        .collect()
}

pub fn silence(n: usize) -> Vec<f32> {
    vec![0.0; n]
}

/// A smooth fbank-like pattern, `[frames][80]`, scaled as CMN'd log-mel features are.
pub fn pattern(frames: usize, a: f64, b: f64) -> Vec<f32> {
    (0..frames * 80)
        .map(|i| {
            let (t, k) = ((i / 80) as f64, (i % 80) as f64);
            (2.0 * (a * k + b * t).sin() * (0.05 * t * k / 80.0).cos()) as f32
        })
        .collect()
}

/// Uniform features in [-1.5, 1.5).
pub fn lcg_features(frames: usize, seed: u32) -> Vec<f32> {
    let mut rng = Lcg(seed);
    (0..frames * 80)
        .map(|_| (3.0 * rng.next() - 1.5) as f32)
        .collect()
}

/// The oracle's Silero input: 3 s of voice, 0.5 s of noise, 0.5 s of silence.
pub fn silero_stream() -> Vec<f32> {
    [
        voice(3 * RATE, 1, 120.0, [700.0, 1_220.0, 2_600.0]),
        noise(RATE / 2, 2, 0.05),
        silence(RATE / 2),
    ]
    .concat()
}

/// The oracle's fbank input: half a second of voice.
pub fn fbank_clip() -> Vec<f32> {
    voice(RATE / 2, 5, 140.0, [600.0, 1_100.0, 2_500.0])
}

/// The oracle's four wake windows: two patterns, noise-like, and zeros.
pub fn wake_windows() -> Vec<Vec<f32>> {
    vec![
        pattern(120, 0.21, 0.13),
        pattern(120, 0.07, 0.31),
        lcg_features(120, 3),
        vec![0.0; 120 * 80],
    ]
}

/// The oracle's ResNet34 input, `[200][80]`.
pub fn resnet_features() -> Vec<f32> {
    let a = pattern(200, 0.17, 0.09);
    let b = lcg_features(200, 11);
    a.iter().zip(&b).map(|(x, y)| x + 0.3 * y).collect()
}

/// Direct path after `delay`, four early reflections, then a diffuse tail decaying 60 dB in `t60`.
pub fn room(
    len: usize,
    delay: usize,
    t60: f64,
    seed: u32,
    reflections: [(usize, f32); 4],
) -> Vec<f32> {
    let mut h = vec![0f32; len];
    h[delay] = 0.6;
    for (at, a) in reflections {
        h[delay + at] += a;
    }
    let mut rng = Lcg(seed);
    for (n, v) in h.iter_mut().enumerate().skip(delay + 240) {
        let age = (n - delay) as f64 / RATE as f64;
        *v += (0.08 * rng.bipolar() * (-6.9 * age / t60).exp()) as f32;
    }
    h
}

pub fn convolve(x: &[f32], h: &[f32]) -> Vec<f32> {
    let taps: Vec<(usize, f32)> = h
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, v)| *v != 0.0)
        .collect();
    let mut y = vec![0f32; x.len()];
    for (n, out) in y.iter_mut().enumerate() {
        let mut acc = 0f32;
        for &(k, v) in &taps {
            if k > n {
                break;
            }
            acc += v * x[n - k];
        }
        *out = acc;
    }
    y
}

/// A Silero-shaped model with seeded weights and no biases, so its output follows the input's
/// energy: real arithmetic, varied probabilities, for the replay tests where the pond has no Silero.
pub fn synthetic_silero(seed: u32) -> pond_ear::ModelFile {
    use pond_ear::convert::vmw::Vmw;
    use pond_ear::engine::silero::{offsets, LAYOUT};
    let (at, total) = offsets();
    let mut rng = Lcg(seed);
    let mut w32: Vec<f32> = (0..total).map(|_| (0.15 * rng.bipolar()) as f32).collect();
    for i in [2usize, 4, 6, 8, 11] {
        w32[at[i]..at[i] + LAYOUT[i]].fill(0.0);
    }
    for v in &mut w32[at[12]..at[12] + LAYOUT[12]] {
        *v = v.abs() * 50.0;
    }
    w32[at[13]] = -3.0;
    let vmw = Vmw {
        arch: pond_ear::engine::model::Arch::Vad,
        frontend: 2,
        emb_dim: 1,
        n_slots: 1,
        feat_dim: 0,
        source_sha256: [0; 32],
        ops: Vec::new(),
        w32,
        i8w: Vec::new(),
        extra: Vec::new(),
    };
    pond_ear::ModelFile::from_vmw(&vmw.to_bytes()).unwrap()
}

/// A small ResNet-shaped speaker model with seeded weights (three convs, TSTP pooling, a 32-dim
/// embedding), for Voice Match plumbing where the pond has no ResNet34.
pub fn synthetic_speaker(seed: u32) -> pond_ear::ModelFile {
    use pond_ear::convert::vmw::{Vmw, W32};
    use pond_ear::engine::model::{Arch, Op, OpKind, FLAG_RELU, NO_I8};
    let mut rng = Lcg(seed);
    let mut w = W32::default();
    let mut tensor = |n: usize, fan_in: usize, w: &mut W32| {
        let s = (1.0 / fan_in as f64).sqrt();
        let v: Vec<f32> = (0..n).map(|_| (s * rng.bipolar()) as f32).collect();
        w.push(&v)
    };
    let conv = |cin: u16, cout: u16, stride: u8, src: u8, dst: u8, wo: u32, bo: u32| Op {
        kind: OpKind::Conv2d,
        flags: FLAG_RELU,
        k: 3,
        stride,
        src,
        dst,
        cin,
        cout,
        w: wo,
        b: bo,
        aux: 0,
        w8: NO_I8,
        s8: 0,
    };
    let mut ops = Vec::new();
    for (cin, cout, stride, src, dst) in [
        (1u16, 16u16, 1u8, 0u8, 1u8),
        (16, 16, 2, 1, 2),
        (16, 32, 2, 2, 1),
    ] {
        let wo = tensor(cout as usize * 9 * cin as usize, 9 * cin as usize, &mut w);
        let bo = tensor(cout as usize, 64, &mut w);
        ops.push(conv(cin, cout, stride, src, dst, wo, bo));
    }
    ops.push(Op {
        kind: OpKind::TstpPool,
        flags: 0,
        k: 0,
        stride: 0,
        src: 1,
        dst: 0,
        cin: 0,
        cout: 0,
        w: 0,
        b: 0,
        aux: 1e-5f32.to_bits(),
        w8: NO_I8,
        s8: 0,
    });
    let pooled = 2 * 20 * 32;
    let wo = tensor(32 * pooled, pooled, &mut w);
    let bo = tensor(32, 64, &mut w);
    ops.push(Op {
        kind: OpKind::Linear,
        flags: 0,
        k: 0,
        stride: 0,
        src: 0,
        dst: 0,
        cin: pooled as u16,
        cout: 32,
        w: wo,
        b: bo,
        aux: 0,
        w8: NO_I8,
        s8: 0,
    });
    let vmw = Vmw {
        arch: Arch::ResNet,
        frontend: 0,
        emb_dim: 32,
        n_slots: 3,
        feat_dim: 80,
        source_sha256: [0; 32],
        ops,
        w32: w.data,
        i8w: Vec::new(),
        extra: Vec::new(),
    };
    pond_ear::ModelFile::from_vmw(&vmw.to_bytes()).unwrap()
}

pub fn golden(name: &str) -> Vec<f32> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let b = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// A model the pond has already fetched, or one named by `env`; `None` means skip, said once.
pub fn pond_model(
    env: &str,
    what: &str,
    at: impl Fn(&std::path::Path) -> PathBuf,
) -> Option<PathBuf> {
    static SAID: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let found = std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(|| pond_ear::locations::data_dir().map(|d| at(&d)))
        .filter(|p| p.exists());
    if found.is_none() {
        let mut said = SAID.lock().unwrap();
        if !said.iter().any(|w| w == what) {
            eprintln!(
                "no {what} model (set {env}, or let the pond fetch it): skipping what needs it"
            );
            said.push(what.to_string());
        }
    }
    found
}

pub fn silero() -> Option<PathBuf> {
    pond_model("POND_EAR_SILERO", "Silero VAD", pond_ear::locations::silero)
}

pub fn speaker() -> Option<PathBuf> {
    pond_model(
        "POND_EAR_SPEAKER",
        "ResNet34 speaker",
        pond_ear::locations::speaker,
    )
}

pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
    for (&x, &y) in a.iter().zip(b) {
        ab += x as f64 * y as f64;
        aa += x as f64 * x as f64;
        bb += y as f64 * y as f64;
    }
    ab / (aa.sqrt() * bb.sqrt()).max(1e-30)
}
