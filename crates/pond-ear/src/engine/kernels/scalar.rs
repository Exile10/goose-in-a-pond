//! Rung 1: scalar f32. Straight loops in the packed layout; the reference.

use super::{ConvArgs, DwArgs};
use crate::engine::model::CO_BLOCK;

pub fn conv2d(a: &ConvArgs, dst: &mut [f32]) {
    let (k, s, p, cin, cout) = (a.k, a.stride, a.pad, a.cin, a.cout);
    let in_row = (a.wi + 2) * cin;
    let out_row = (a.wo + 2) * cout;
    let block_w = k * k * cin * CO_BLOCK;

    for cb in 0..cout / CO_BLOCK {
        let wb = &a.w[cb * block_w..(cb + 1) * block_w];
        let bias = &a.b[cb * CO_BLOCK..(cb + 1) * CO_BLOCK];
        for y in 0..a.ho {
            for x in 0..a.wo {
                let mut acc = [0f32; CO_BLOCK];
                acc.copy_from_slice(bias);
                for ky in 0..k {
                    let row = s * y + ky + 1 - p;
                    for kx in 0..k {
                        let col = s * x + kx + 1 - p;
                        let px = &a.src[row * in_row + col * cin..][..cin];
                        let wt = &wb[(ky * k + kx) * cin * CO_BLOCK..][..cin * CO_BLOCK];
                        for (ci, &v) in px.iter().enumerate() {
                            let wc = &wt[ci * CO_BLOCK..(ci + 1) * CO_BLOCK];
                            for j in 0..CO_BLOCK {
                                acc[j] += v * wc[j];
                            }
                        }
                    }
                }
                let o = &mut dst[(y + 1) * out_row + (x + 1) * cout + cb * CO_BLOCK..][..CO_BLOCK];
                for j in 0..CO_BLOCK {
                    let mut r = acc[j];
                    if a.accum {
                        r += o[j];
                    }
                    if a.relu && r < 0.0 {
                        r = 0.0;
                    }
                    o[j] = r;
                }
            }
        }
    }
}

pub fn depthwise(a: &DwArgs, dst: &mut [f32]) {
    let (k, s, p, c) = (a.k, a.stride, a.pad, a.c);
    let in_row = (a.wi + 2) * c;
    let out_row = (a.wo + 2) * c;
    for y in 0..a.ho {
        for x in 0..a.wo {
            let o = &mut dst[(y + 1) * out_row + (x + 1) * c..][..c];
            o.copy_from_slice(&a.b[..c]);
            for ky in 0..k {
                let row = s * y + ky + 1 - p;
                for kx in 0..k {
                    let col = s * x + kx + 1 - p;
                    let px = &a.src[row * in_row + col * c..][..c];
                    let wt = &a.w[(ky * k + kx) * c..][..c];
                    for ch in 0..c {
                        o[ch] += px[ch] * wt[ch];
                    }
                }
            }
            if a.relu {
                for v in o.iter_mut() {
                    if *v < 0.0 {
                        *v = 0.0;
                    }
                }
            }
        }
    }
}
