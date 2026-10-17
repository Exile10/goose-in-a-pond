//! Rung 3: int8 `SDOT`, 16 MACs per instruction on the f32 rung's tile. Weights are int8 per output
//! channel offline; activations int8 per tensor at run time, scaled by their own max.

use core::arch::aarch64::*;
use core::arch::asm;

use super::ConvArgs;
use crate::engine::model::CO_BLOCK;

const PX: usize = 8;

/// Quantise a halo-layout activation into `q`, returning `(scale, offset)`. A non-negative tensor
/// is stored offset by -128 to use all 256 levels (symmetric int8 measured 0.983 worst cosine).
pub fn quantize(x: &[f32], q: &mut [i8]) -> (f32, bool) {
    // SAFETY: NEON is mandatory on aarch64; loads stay inside `x` (vector body, scalar tail).
    unsafe {
        let n = x.len();
        let p = x.as_ptr();
        let (mut hi, mut lo) = (vdupq_n_f32(0.0), vdupq_n_f32(0.0));
        let mut i = 0;
        while i + 4 <= n {
            let v = vld1q_f32(p.add(i));
            hi = vmaxq_f32(hi, v);
            lo = vminq_f32(lo, v);
            i += 4;
        }
        let (mut max, mut min) = (vmaxvq_f32(hi), vminvq_f32(lo));
        while i < n {
            max = max.max(x[i]);
            min = min.min(x[i]);
            i += 1;
        }
        let offset = min >= 0.0;
        // The max, not a percentile: clipping the top 0.01% of a ReLU tensor raised the embedding
        // error from 12% to 17%, so the rare large activations carry the signal.
        let range = if offset { max } else { max.max(-min) };
        let scale = if range > 0.0 {
            range / if offset { 255.0 } else { 127.0 }
        } else {
            1.0
        };
        let inv = vdupq_n_f32(1.0 / scale);
        let shift = vdupq_n_s32(if offset { 128 } else { 0 });
        let o = q.as_mut_ptr();
        let mut i = 0;
        while i + 16 <= n {
            // Saturating narrows clamp to [-128, 127], which is exactly the range.
            let a = vsubq_s32(vcvtnq_s32_f32(vmulq_f32(vld1q_f32(p.add(i)), inv)), shift);
            let b = vsubq_s32(
                vcvtnq_s32_f32(vmulq_f32(vld1q_f32(p.add(i + 4)), inv)),
                shift,
            );
            let c = vsubq_s32(
                vcvtnq_s32_f32(vmulq_f32(vld1q_f32(p.add(i + 8)), inv)),
                shift,
            );
            let d = vsubq_s32(
                vcvtnq_s32_f32(vmulq_f32(vld1q_f32(p.add(i + 12)), inv)),
                shift,
            );
            let ab = vcombine_s16(vqmovn_s32(a), vqmovn_s32(b));
            let cd = vcombine_s16(vqmovn_s32(c), vqmovn_s32(d));
            vst1q_s8(o.add(i), vcombine_s8(vqmovn_s16(ab), vqmovn_s16(cd)));
            i += 16;
        }
        let sh = if offset { 128 } else { 0 };
        while i < n {
            q[i] = (vcvtns_s32_f32(x[i] / scale) - sh).clamp(-128, 127) as i8;
            i += 1;
        }
        (scale, offset)
    }
}

pub struct I8Args<'a> {
    /// The quantised source, same halo layout as `ConvArgs::src`.
    pub src: &'a [i8],
    pub src_scale: f32,
    /// Packed `[cout/8][k*k][cin/4][8][4]`.
    pub w: &'a [i8],
    /// Per output channel.
    pub w_scale: &'a [f32],
    /// Per output channel: the sum of its int8 weights.
    pub w_sum: &'a [f32],
    /// `src` is stored offset by -128 (see `quantize`).
    pub offset: bool,
}

pub fn conv2d(a: &ConvArgs, q: &I8Args, dst: &mut [f32]) {
    debug_assert!(a.cin.is_multiple_of(16));
    debug_assert!(q.src.len() >= (a.hi + 2) * (a.wi + 2) * a.cin);
    debug_assert!(dst.len() >= (a.ho + 2) * (a.wo + 2) * a.cout);
    // SAFETY: the caller checked `dotprod`; indices as in the f32 rung.
    unsafe { conv(a, q, dst.as_mut_ptr()) }
}

struct Geo {
    k: usize,
    s: usize,
    p: usize,
    cin: usize,
    cout: usize,
    in_row: usize,
    out_row: usize,
    relu: bool,
    accum: bool,
}

#[target_feature(enable = "neon,dotprod")]
unsafe fn conv(a: &ConvArgs, q: &I8Args, out: *mut f32) {
    let g = Geo {
        k: a.k,
        s: a.stride,
        p: a.pad,
        cin: a.cin,
        cout: a.cout,
        in_row: (a.wi + 2) * a.cin,
        out_row: (a.wo + 2) * a.cout,
        relu: a.relu,
        accum: a.accum,
    };
    let block_w = g.k * g.k * g.cin * CO_BLOCK;
    let src = q.src.as_ptr();
    for cb in 0..g.cout / CO_BLOCK {
        let wb = q.w.as_ptr().add(cb * block_w);
        // Requantisation: acc * (src_scale * w_scale[co]) + bias[co].
        let sx = vdupq_n_f32(q.src_scale);
        let ws = q.w_scale.as_ptr().add(cb * CO_BLOCK);
        let scale = [
            vmulq_f32(sx, vld1q_f32(ws)),
            vmulq_f32(sx, vld1q_f32(ws.add(4))),
        ];
        let bias = [
            vld1q_f32(a.b.as_ptr().add(cb * CO_BLOCK)),
            vld1q_f32(a.b.as_ptr().add(cb * CO_BLOCK + 4)),
        ];
        // Undo the -128 input offset: + 128 * sum(w) per output channel.
        let k128 = vdupq_n_f32(if q.offset { 128.0 } else { 0.0 });
        let sums = q.w_sum.as_ptr().add(cb * CO_BLOCK);
        let corr = [
            vmulq_f32(k128, vld1q_f32(sums)),
            vmulq_f32(k128, vld1q_f32(sums.add(4))),
        ];
        for y in 0..a.ho {
            let mut x = 0;
            while x + PX <= a.wo {
                tile::<PX>(&g, src, wb, &scale, &bias, &corr, out, cb, y, x);
                x += PX;
            }
            match a.wo - x {
                0 => {}
                1 => tile::<1>(&g, src, wb, &scale, &bias, &corr, out, cb, y, x),
                2 => tile::<2>(&g, src, wb, &scale, &bias, &corr, out, cb, y, x),
                3 => tile::<3>(&g, src, wb, &scale, &bias, &corr, out, cb, y, x),
                4 => tile::<4>(&g, src, wb, &scale, &bias, &corr, out, cb, y, x),
                5 => tile::<5>(&g, src, wb, &scale, &bias, &corr, out, cb, y, x),
                6 => tile::<6>(&g, src, wb, &scale, &bias, &corr, out, cb, y, x),
                _ => tile::<7>(&g, src, wb, &scale, &bias, &corr, out, cb, y, x),
            }
        }
    }
}

/// `acc += w . x[group lane]`, one SDOT by element: inline asm because the dot-product intrinsics
/// are unstable in Rust, in a function compiled with `dotprod`, which the caller must check.
macro_rules! sdot {
    ($acc:expr, $w:expr, $x:expr, $lane:literal) => {
        asm!(concat!("sdot {a:v}.4s, {w:v}.16b, {x:v}.4b[", $lane, "]"),
             a = inout(vreg) $acc, w = in(vreg) $w, x = in(vreg) $x,
             options(pure, nomem, nostack, preserves_flags))
    };
}

/// One input-channel group for every pixel: two weight vectors (co 0-3, 4-7).
macro_rules! group {
    ($n:expr, $L:literal, $acc:ident, $v:ident, $wt:ident, $g:expr) => {{
        let w0 = vld1q_s8($wt.add(($g + $L) * 32));
        let w1 = vld1q_s8($wt.add(($g + $L) * 32 + 16));
        for q in 0..$n {
            sdot!($acc[q][0], w0, $v[q], $L);
            sdot!($acc[q][1], w1, $v[q], $L);
        }
    }};
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn tile<const N: usize>(
    g: &Geo,
    src: *const i8,
    wb: *const i8,
    scale: &[float32x4_t; 2],
    bias: &[float32x4_t; 2],
    corr: &[float32x4_t; 2],
    out: *mut f32,
    cb: usize,
    y: usize,
    x: usize,
) {
    let mut acc = [[vdupq_n_s32(0); 2]; N];
    let px_stride = g.s * g.cin;
    for ky in 0..g.k {
        let row = g.s * y + ky + 1 - g.p;
        for kx in 0..g.k {
            let col = g.s * x + kx + 1 - g.p;
            let base = src.add(row * g.in_row + col * g.cin);
            let wt = wb.add((ky * g.k + kx) * g.cin * CO_BLOCK);
            let mut ci = 0;
            while ci < g.cin {
                let mut v = [vdupq_n_s8(0); N];
                for (q, vq) in v.iter_mut().enumerate() {
                    *vq = vld1q_s8(base.add(q * px_stride + ci));
                }
                let gi = ci / 4;
                group!(N, 0, acc, v, wt, gi);
                group!(N, 1, acc, v, wt, gi);
                group!(N, 2, acc, v, wt, gi);
                group!(N, 3, acc, v, wt, gi);
                ci += 16;
            }
        }
    }
    let zero = vdupq_n_f32(0.0);
    for (q, a) in acc.iter().enumerate() {
        let o = out.add((y + 1) * g.out_row + (x + q + 1) * g.cout + cb * CO_BLOCK);
        let mut r0 = vfmaq_f32(bias[0], vaddq_f32(vcvtq_f32_s32(a[0]), corr[0]), scale[0]);
        let mut r1 = vfmaq_f32(bias[1], vaddq_f32(vcvtq_f32_s32(a[1]), corr[1]), scale[1]);
        if g.accum {
            r0 = vaddq_f32(r0, vld1q_f32(o));
            r1 = vaddq_f32(r1, vld1q_f32(o.add(4)));
        }
        if g.relu {
            r0 = vmaxq_f32(r0, zero);
            r1 = vmaxq_f32(r1, zero);
        }
        vst1q_f32(o, r0);
        vst1q_f32(o.add(4), r1);
    }
}
