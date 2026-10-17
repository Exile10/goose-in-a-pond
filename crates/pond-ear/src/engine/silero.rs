//! Silero VAD v5's 16 kHz branch by hand: 512 new samples plus the 64 before them (the official
//! wrapper's context) in, a speech probability out; ~0.7 MMAC a call, nothing allocates.

use super::kernels::Rung;
use super::math;

pub const RATE: usize = 16_000;
/// New samples per call.
pub const CHUNK: usize = 512;
/// Samples carried over from the previous call.
pub const CONTEXT: usize = 64;
const INPUT: usize = CHUNK + CONTEXT;
const PAD: usize = 64;
const PADDED: usize = INPUT + PAD;
const WIN: usize = 256;
const HOP: usize = 128;
const T0: usize = (PADDED - WIN) / HOP + 1;
const SPEC: usize = 258;
const BINS: usize = 129;
const C1: usize = 128;
const C2: usize = 64;
const C3: usize = 64;
const C4: usize = 128;
pub const HIDDEN: usize = 128;
const T1: usize = T0; // k3 s1 p1
const T2: usize = (T1 + 2 - 3) / 2 + 1; // k3 s2 p1
const T3: usize = (T2 + 2 - 3) / 2 + 1;
const T4: usize = T3;

/// The weights in `w32`, each on a 16-float boundary, in ONNX layouts: conv `[cout][cin][k]`,
/// LSTM `W`/`R` `[4 * hidden][in]` with gates i, o, f, c, and `B` = `[Wb; Rb]`.
pub const LAYOUT: [usize; 14] = [
    SPEC * WIN,          // stft
    C1 * BINS * 3,       // conv1 w
    C1,                  // conv1 b
    C2 * C1 * 3,         // conv2 w
    C2,                  // conv2 b
    C3 * C2 * 3,         // conv3 w
    C3,                  // conv3 b
    C4 * C3 * 3,         // conv4 w
    C4,                  // conv4 b
    4 * HIDDEN * C4,     // lstm W
    4 * HIDDEN * HIDDEN, // lstm R
    8 * HIDDEN,          // lstm B
    HIDDEN,              // out w
    1,                   // out b
];

/// Start of each `LAYOUT` tensor in `w32`, and the total length.
pub const fn offsets() -> ([usize; 14], usize) {
    let mut off = [0usize; 14];
    let mut at = 0usize;
    let mut i = 0;
    while i < 14 {
        at = at.div_ceil(16) * 16;
        off[i] = at;
        at += LAYOUT[i];
        i += 1;
    }
    (off, at)
}

/// What carries over between calls.
#[derive(Clone)]
pub struct State {
    pub h: [f32; HIDDEN],
    pub c: [f32; HIDDEN],
    pub context: [f32; CONTEXT],
}

impl State {
    pub const fn new() -> State {
        State {
            h: [0.0; HIDDEN],
            c: [0.0; HIDDEN],
            context: [0.0; CONTEXT],
        }
    }
}

impl Default for State {
    fn default() -> Self {
        State::new()
    }
}

/// Activations of one call.
pub struct Scratch {
    x: [f32; PADDED],
    spec: [f32; SPEC * T0],
    mag: [f32; BINS * T0],
    a1: [f32; C1 * T1],
    a2: [f32; C2 * T2],
    a3: [f32; C3 * T3],
    a4: [f32; C4 * T4],
    col: [f32; BINS * 3],
    gates: [f32; 4 * HIDDEN],
}

impl Scratch {
    pub const fn new() -> Scratch {
        Scratch {
            x: [0.0; PADDED],
            spec: [0.0; SPEC * T0],
            mag: [0.0; BINS * T0],
            a1: [0.0; C1 * T1],
            a2: [0.0; C2 * T2],
            a3: [0.0; C3 * T3],
            a4: [0.0; C4 * T4],
            col: [0.0; BINS * 3],
            gates: [0.0; 4 * HIDDEN],
        }
    }
}

impl Default for Scratch {
    fn default() -> Self {
        Scratch::new()
    }
}

pub const MACS_PER_CALL: u64 = (SPEC * WIN * T0
    + C1 * BINS * 3 * T1
    + C2 * C1 * 3 * T2
    + C3 * C2 * 3 * T3
    + C4 * C3 * 3 * T4
    + 4 * HIDDEN * (C4 + HIDDEN)
    + HIDDEN) as u64;

fn dot(rung: Rung, a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    #[cfg(target_arch = "aarch64")]
    if rung != Rung::Scalar {
        return dot_neon(a, b);
    }
    let _ = rung;
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(target_arch = "aarch64")]
fn dot_neon(a: &[f32], b: &[f32]) -> f32 {
    use core::arch::aarch64::*;
    let n = a.len().min(b.len());
    let mut i = 0;
    // SAFETY: NEON is baseline on aarch64; every load reads 4 floats at i < n - 3.
    unsafe {
        let (mut s0, mut s1, mut s2, mut s3) = (
            vdupq_n_f32(0.0),
            vdupq_n_f32(0.0),
            vdupq_n_f32(0.0),
            vdupq_n_f32(0.0),
        );
        while i + 16 <= n {
            s0 = vfmaq_f32(
                s0,
                vld1q_f32(a.as_ptr().add(i)),
                vld1q_f32(b.as_ptr().add(i)),
            );
            s1 = vfmaq_f32(
                s1,
                vld1q_f32(a.as_ptr().add(i + 4)),
                vld1q_f32(b.as_ptr().add(i + 4)),
            );
            s2 = vfmaq_f32(
                s2,
                vld1q_f32(a.as_ptr().add(i + 8)),
                vld1q_f32(b.as_ptr().add(i + 8)),
            );
            s3 = vfmaq_f32(
                s3,
                vld1q_f32(a.as_ptr().add(i + 12)),
                vld1q_f32(b.as_ptr().add(i + 12)),
            );
            i += 16;
        }
        while i + 4 <= n {
            s0 = vfmaq_f32(
                s0,
                vld1q_f32(a.as_ptr().add(i)),
                vld1q_f32(b.as_ptr().add(i)),
            );
            i += 4;
        }
        let mut s = vaddvq_f32(vaddq_f32(vaddq_f32(s0, s1), vaddq_f32(s2, s3)));
        while i < n {
            s += a[i] * b[i];
            i += 1;
        }
        s
    }
}

/// Conv1d over a channel-major `[cin][t_in]`, kernel 3, padding 1, then relu, into `[cout][t_out]`:
/// each output is one dot of the weight row with the matching `[cin][3]` column.
#[allow(clippy::too_many_arguments)]
fn conv3_relu(
    rung: Rung,
    w: &[f32],
    b: &[f32],
    x: &[f32],
    cin: usize,
    t_in: usize,
    stride: usize,
    cout: usize,
    t_out: usize,
    col: &mut [f32],
    y: &mut [f32],
) {
    for t in 0..t_out {
        for ci in 0..cin {
            for k in 0..3 {
                let src = (t * stride + k) as isize - 1;
                col[ci * 3 + k] = if src >= 0 && (src as usize) < t_in {
                    x[ci * t_in + src as usize]
                } else {
                    0.0
                };
            }
        }
        let col = &col[..cin * 3];
        for co in 0..cout {
            let v = b[co] + dot(rung, &w[co * cin * 3..(co + 1) * cin * 3], col);
            y[co * t_out + t] = if v > 0.0 { v } else { 0.0 };
        }
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + math::exp(-x))
}

fn tanh(x: f32) -> f32 {
    2.0 * sigmoid(2.0 * x) - 1.0
}

/// One call on exactly `CHUNK` new samples in [-1, 1], with the model's `w32` (see `LAYOUT`).
pub fn step(rung: Rung, w: &[f32], st: &mut State, s: &mut Scratch, chunk: &[f32]) -> f32 {
    debug_assert_eq!(chunk.len(), CHUNK);
    let (o, total) = offsets();
    debug_assert!(w.len() >= total);
    let t = |i: usize| &w[o[i]..o[i] + LAYOUT[i]];

    // The previous call's last 64 samples, then the chunk, then the graph's reflected right edge.
    s.x[..CONTEXT].copy_from_slice(&st.context);
    s.x[CONTEXT..INPUT].copy_from_slice(chunk);
    for i in 0..PAD {
        s.x[INPUT + i] = s.x[INPUT - 2 - i];
    }
    st.context.copy_from_slice(&s.x[INPUT - CONTEXT..INPUT]);

    let stft = t(0);
    for f in 0..T0 {
        let frame = &s.x[f * HOP..f * HOP + WIN];
        for c in 0..SPEC {
            s.spec[c * T0 + f] = dot(rung, &stft[c * WIN..(c + 1) * WIN], frame);
        }
    }
    for bin in 0..BINS {
        for f in 0..T0 {
            let (re, im) = (s.spec[bin * T0 + f], s.spec[(bin + BINS) * T0 + f]);
            s.mag[bin * T0 + f] = math::sqrt(re * re + im * im);
        }
    }

    conv3_relu(
        rung,
        t(1),
        t(2),
        &s.mag,
        BINS,
        T0,
        1,
        C1,
        T1,
        &mut s.col,
        &mut s.a1,
    );
    conv3_relu(
        rung,
        t(3),
        t(4),
        &s.a1,
        C1,
        T1,
        2,
        C2,
        T2,
        &mut s.col,
        &mut s.a2,
    );
    conv3_relu(
        rung,
        t(5),
        t(6),
        &s.a2,
        C2,
        T2,
        2,
        C3,
        T3,
        &mut s.col,
        &mut s.a3,
    );
    conv3_relu(
        rung,
        t(7),
        t(8),
        &s.a3,
        C3,
        T3,
        1,
        C4,
        T4,
        &mut s.col,
        &mut s.a4,
    );

    // ONNX's LSTM gate order is i, o, f, c.
    let (wi, ri, bias) = (t(9), t(10), t(11));
    for g in 0..4 * HIDDEN {
        s.gates[g] = bias[g]
            + bias[4 * HIDDEN + g]
            + dot(rung, &wi[g * C4..(g + 1) * C4], &s.a4)
            + dot(rung, &ri[g * HIDDEN..(g + 1) * HIDDEN], &st.h);
    }
    for j in 0..HIDDEN {
        let i = sigmoid(s.gates[j]);
        let og = sigmoid(s.gates[HIDDEN + j]);
        let f = sigmoid(s.gates[2 * HIDDEN + j]);
        let c = tanh(s.gates[3 * HIDDEN + j]);
        st.c[j] = f * st.c[j] + i * c;
        st.h[j] = og * tanh(st.c[j]);
    }

    let (ow, ob) = (t(12), t(13));
    let mut v = ob[0];
    for (&w, &h) in ow.iter().zip(&st.h) {
        v += w * if h > 0.0 { h } else { 0.0 };
    }
    sigmoid(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_follow_the_graph() {
        assert_eq!((T0, T1, T2, T3, T4), (4, 4, 2, 1, 1));
        let (_, total) = offsets();
        assert!(total > 300_000 && total < 320_000, "{total}");
        const { assert!(MACS_PER_CALL > 600_000 && MACS_PER_CALL < 800_000) };
    }

    #[test]
    fn neon_and_scalar_dots_agree() {
        let a: [f32; 387] = core::array::from_fn(|i| ((i * 7919) % 101) as f32 / 50.0 - 1.0);
        let b: [f32; 387] = core::array::from_fn(|i| ((i * 104729) % 97) as f32 / 48.0 - 1.0);
        let s = dot(Rung::Scalar, &a, &b);
        for &r in Rung::ALL {
            assert!((dot(r, &a, &b) - s).abs() < 1e-3);
        }
    }
}
