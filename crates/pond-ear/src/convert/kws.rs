//! Wake-word CNN ONNX graph (sequential depthwise-separable, batch norm folded) -> the engine's
//! program: each Conv+Relu one op, then `GapPool` and `Linear`; two slots ping-pong.

use std::collections::HashMap;

use crate::engine::model::{Arch, Op, OpKind, CO_BLOCK, FLAG_RELU, I8_CI_STEP, NO_I8};

use super::proto::{Graph, Node};
use super::resnet::pack_i8_into;
use super::vmw::{Vmw, W32};

pub fn convert(g: &Graph, source_sha256: [u8; 32]) -> Result<Vmw, String> {
    let mut consumers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        for inp in &n.inputs {
            consumers.entry(inp.as_str()).or_default().push(i);
        }
    }
    let only = |t: &str| -> Option<&Node> {
        match consumers.get(t).map(|v| v.as_slice()) {
            Some([i]) => Some(&g.nodes[*i]),
            _ => None,
        }
    };
    let feats = g.inputs.first().ok_or("no graph input")?;
    let tr = only(feats)
        .filter(|n| n.op == "Transpose" && n.ints("perm") == [0, 2, 1])
        .ok_or("expected Transpose(0,2,1) on the input")?;
    let un = only(&tr.outputs[0])
        .filter(|n| n.op == "Unsqueeze")
        .ok_or("expected Unsqueeze after Transpose")?;

    let (mut ops, mut w32, mut i8w) = (Vec::new(), W32::default(), Vec::new());
    let mut cur = un.outputs[0].clone();
    let mut slot = 0u8;
    let emb_dim = loop {
        let n = only(&cur).ok_or_else(|| format!("{cur}: expected exactly one consumer"))?;
        match n.op.as_str() {
            "Conv" => {
                let w = g.init.get(&n.inputs[1]).ok_or("conv weight not constant")?;
                let (cout, cin_per_group, k) =
                    (w.dims[0] as usize, w.dims[1] as usize, w.dims[2] as usize);
                let group = n.int("group").unwrap_or(1) as usize;
                let stride = n.ints("strides").first().copied().unwrap_or(1) as usize;
                if n.ints("strides").iter().any(|&s| s as usize != stride)
                    || n.ints("pads").iter().any(|&p| p as usize != k / 2)
                    || n.ints("dilations").iter().any(|&d| d != 1)
                    || w.dims[3] as usize != k
                {
                    return Err(format!("{}: unsupported conv geometry", n.outputs[0]));
                }
                let bias = match n.inputs.get(2) {
                    Some(b) => g.init[b].f32s(),
                    None => vec![0.0; cout],
                };
                let src_w = w.f32s();
                let mut out = n.outputs[0].clone();
                let mut flags = 0;
                if let Some(r) = only(&out).filter(|r| r.op == "Relu") {
                    flags |= FLAG_RELU;
                    out = r.outputs[0].clone();
                }
                let dst = 1 - slot;
                if group == 1 {
                    let cin = cin_per_group;
                    if cout % CO_BLOCK != 0 {
                        return Err(format!(
                            "{}: cout {cout} not a multiple of {CO_BLOCK}",
                            n.outputs[0]
                        ));
                    }
                    let mut packed = vec![0f32; src_w.len()];
                    for co in 0..cout {
                        for ci in 0..cin {
                            for tap in 0..k * k {
                                packed[((co / CO_BLOCK * k * k + tap) * cin + ci) * CO_BLOCK
                                    + co % CO_BLOCK] = src_w[(co * cin + ci) * k * k + tap];
                            }
                        }
                    }
                    let (w8, s8) = if cin % I8_CI_STEP == 0 {
                        pack_i8_into(&mut i8w, &mut w32, &src_w, cout, cin, k)
                    } else {
                        (NO_I8, 0)
                    };
                    let wo = w32.push(&packed);
                    let bo = w32.push(&bias);
                    ops.push(Op {
                        kind: OpKind::Conv2d,
                        flags,
                        k: k as u8,
                        stride: stride as u8,
                        src: slot,
                        dst,
                        cin: cin as u16,
                        cout: cout as u16,
                        w: wo,
                        b: bo,
                        aux: 0,
                        w8,
                        s8,
                    });
                } else if group == cout && cin_per_group == 1 {
                    // Depthwise: [c][1][k][k] -> [k*k][c].
                    let mut packed = vec![0f32; src_w.len()];
                    for c in 0..cout {
                        for tap in 0..k * k {
                            packed[tap * cout + c] = src_w[c * k * k + tap];
                        }
                    }
                    let wo = w32.push(&packed);
                    let bo = w32.push(&bias);
                    ops.push(Op {
                        kind: OpKind::Depthwise,
                        flags,
                        k: k as u8,
                        stride: stride as u8,
                        src: slot,
                        dst,
                        cin: cout as u16,
                        cout: cout as u16,
                        w: wo,
                        b: bo,
                        aux: 0,
                        w8: NO_I8,
                        s8: 0,
                    });
                } else {
                    return Err(format!(
                        "{}: grouped conv that is neither dense nor depthwise",
                        n.outputs[0]
                    ));
                }
                slot = dst;
                cur = out;
            }
            "GlobalAveragePool" => {
                ops.push(Op {
                    kind: OpKind::GapPool,
                    flags: 0,
                    k: 0,
                    stride: 0,
                    src: slot,
                    dst: 0,
                    cin: 0,
                    cout: 0,
                    w: 0,
                    b: 0,
                    aux: 0,
                    w8: NO_I8,
                    s8: 0,
                });
                cur = n.outputs[0].clone();
            }
            "Flatten" | "Reshape" | "Squeeze" => cur = n.outputs[0].clone(),
            "Gemm" => {
                if n.int("transB") != Some(1) {
                    return Err("Gemm must be transB=1".into());
                }
                let w = &g.init[&n.inputs[1]];
                let (cout, cin) = (w.dims[0] as usize, w.dims[1] as usize);
                let wo = w32.push(&w.f32s());
                let bo = w32.push(&g.init[&n.inputs[2]].f32s());
                ops.push(Op {
                    kind: OpKind::Linear,
                    flags: 0,
                    k: 0,
                    stride: 0,
                    src: 0,
                    dst: 0,
                    cin: cin as u16,
                    cout: cout as u16,
                    w: wo,
                    b: bo,
                    aux: 0,
                    w8: NO_I8,
                    s8: 0,
                });
                if !g.outputs.iter().any(|o| *o == n.outputs[0]) {
                    return Err("Gemm must be the graph output".into());
                }
                break cout;
            }
            other => return Err(format!("unsupported op {other} in a wake-word CNN")),
        }
    };
    Ok(Vmw {
        arch: Arch::Kws,
        frontend: 0,
        emb_dim,
        n_slots: 2,
        feat_dim: 80,
        source_sha256,
        ops,
        w32: w32.data,
        i8w,
        extra: Vec::new(),
    })
}
