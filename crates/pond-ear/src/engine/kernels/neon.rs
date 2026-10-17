//! Rung 2: NEON f32 direct convolution, an 8-pixel x 8-channel tile in 16 accumulators: 64 FMAs
//! per 16 loads, 26 of 32 vector registers live, no im2col (the halo layout makes taps contiguous).

use core::arch::aarch64::*;

use super::{ConvArgs, DwArgs};
use crate::engine::model::CO_BLOCK;

const PX: usize = 8;

pub fn conv2d(a: &ConvArgs, dst: &mut [f32]) {
    // In bounds by construction of the halo layout and the plan; the scalar rung indexes the same
    // elements with checks, and the parity suite runs both on every op.
    debug_assert!(dst.len() >= (a.ho + 2) * (a.wo + 2) * a.cout);
    debug_assert!(a.src.len() >= (a.hi + 2) * (a.wi + 2) * a.cin);
    // SAFETY: NEON is mandatory on aarch64; see the bounds note above.
    unsafe { conv(a, dst.as_mut_ptr()) }
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

#[target_feature(enable = "neon")]
unsafe fn conv(a: &ConvArgs, out: *mut f32) {
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
    let src = a.src.as_ptr();
    for cb in 0..g.cout / CO_BLOCK {
        let wb = a.w.as_ptr().add(cb * block_w);
        let bias = a.b.as_ptr().add(cb * CO_BLOCK);
        for y in 0..a.ho {
            let mut x = 0;
            while x + PX <= a.wo {
                tile::<PX>(&g, src, wb, bias, out, cb, y, x);
                x += PX;
            }
            match a.wo - x {
                0 => {}
                1 => tile::<1>(&g, src, wb, bias, out, cb, y, x),
                2 => tile::<2>(&g, src, wb, bias, out, cb, y, x),
                3 => tile::<3>(&g, src, wb, bias, out, cb, y, x),
                4 => tile::<4>(&g, src, wb, bias, out, cb, y, x),
                5 => tile::<5>(&g, src, wb, bias, out, cb, y, x),
                6 => tile::<6>(&g, src, wb, bias, out, cb, y, x),
                _ => tile::<7>(&g, src, wb, bias, out, cb, y, x),
            }
        }
    }
}

/// One FMA lane step for every pixel of the tile: acc[p] += w * in[p][LANE].
macro_rules! lane {
    ($n:expr, $L:literal, $acc:ident, $v:ident, $wt:ident, $ci:ident) => {{
        let w0 = vld1q_f32($wt.add(($ci + $L) * CO_BLOCK));
        let w1 = vld1q_f32($wt.add(($ci + $L) * CO_BLOCK + 4));
        for q in 0..$n {
            $acc[q][0] = vfmaq_laneq_f32::<$L>($acc[q][0], w0, $v[q]);
            $acc[q][1] = vfmaq_laneq_f32::<$L>($acc[q][1], w1, $v[q]);
        }
    }};
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn tile<const N: usize>(
    g: &Geo,
    src: *const f32,
    wb: *const f32,
    bias: *const f32,
    out: *mut f32,
    cb: usize,
    y: usize,
    x: usize,
) {
    let b0 = vld1q_f32(bias);
    let b1 = vld1q_f32(bias.add(4));
    let mut acc = [[b0, b1]; N];
    let px_stride = g.s * g.cin;
    for ky in 0..g.k {
        let row = g.s * y + ky + 1 - g.p;
        for kx in 0..g.k {
            let col = g.s * x + kx + 1 - g.p;
            let base = src.add(row * g.in_row + col * g.cin);
            let wt = wb.add((ky * g.k + kx) * g.cin * CO_BLOCK);
            let mut ci = 0;
            while ci + 4 <= g.cin {
                let mut v = [vdupq_n_f32(0.0); N];
                for (q, vq) in v.iter_mut().enumerate() {
                    *vq = vld1q_f32(base.add(q * px_stride + ci));
                }
                lane!(N, 0, acc, v, wt, ci);
                lane!(N, 1, acc, v, wt, ci);
                lane!(N, 2, acc, v, wt, ci);
                lane!(N, 3, acc, v, wt, ci);
                ci += 4;
            }
            // cin not a multiple of 4: only the stem (cin = 1) takes this path.
            while ci < g.cin {
                let w0 = vld1q_f32(wt.add(ci * CO_BLOCK));
                let w1 = vld1q_f32(wt.add(ci * CO_BLOCK + 4));
                for (q, a) in acc.iter_mut().enumerate() {
                    let vv = vdupq_n_f32(*base.add(q * px_stride + ci));
                    a[0] = vfmaq_f32(a[0], w0, vv);
                    a[1] = vfmaq_f32(a[1], w1, vv);
                }
                ci += 1;
            }
        }
    }
    let zero = vdupq_n_f32(0.0);
    for (q, a) in acc.iter().enumerate() {
        let o = out.add((y + 1) * g.out_row + (x + q + 1) * g.cout + cb * CO_BLOCK);
        let (mut r0, mut r1) = (a[0], a[1]);
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

/// Depthwise: four channels per vector, nine taps of FMA, bias and ReLU fused.
pub fn depthwise(a: &DwArgs, dst: &mut [f32]) {
    debug_assert!(a.c.is_multiple_of(4));
    debug_assert!(dst.len() >= (a.ho + 2) * (a.wo + 2) * a.c);
    // SAFETY: NEON is mandatory on aarch64; indices as in the scalar rung.
    unsafe { dw(a, dst.as_mut_ptr()) }
}

#[target_feature(enable = "neon")]
unsafe fn dw(a: &DwArgs, out: *mut f32) {
    let (k, s, p, c) = (a.k, a.stride, a.pad, a.c);
    let in_row = (a.wi + 2) * c;
    let out_row = (a.wo + 2) * c;
    let (src, w, b) = (a.src.as_ptr(), a.w.as_ptr(), a.b.as_ptr());
    let zero = vdupq_n_f32(0.0);
    for y in 0..a.ho {
        for x in 0..a.wo {
            let o = out.add((y + 1) * out_row + (x + 1) * c);
            let mut ch = 0;
            while ch < c {
                let mut acc = vld1q_f32(b.add(ch));
                for ky in 0..k {
                    let row = s * y + ky + 1 - p;
                    for kx in 0..k {
                        let col = s * x + kx + 1 - p;
                        acc = vfmaq_f32(
                            acc,
                            vld1q_f32(src.add(row * in_row + col * c + ch)),
                            vld1q_f32(w.add((ky * k + kx) * c + ch)),
                        );
                    }
                }
                if a.relu {
                    acc = vmaxq_f32(acc, zero);
                }
                vst1q_f32(o.add(ch), acc);
                ch += 4;
            }
        }
    }
}
