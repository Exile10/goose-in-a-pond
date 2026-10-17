//! WeSpeaker ResNet (TSTP pooling) ONNX graph -> the engine's program: ReLUs and residual adds
//! fused into conv epilogues, the pooling tail into one `TstpPool`, `Gemm - mean` into `Linear`.

use std::collections::{HashMap, HashSet};

use crate::engine::model::{
    Arch, Op, OpKind, CO_BLOCK, FLAG_ACCUM, FLAG_RELU, I8_CI_STEP, MAX_SLOTS, NO_I8,
};

use super::proto::{Graph, Node};
use super::vmw::{Vmw, W32};

struct Conv<'g> {
    node: &'g Node,
    k: usize,
    stride: usize,
}

struct Builder<'g> {
    g: &'g Graph,
    uses: HashMap<&'g str, usize>,
    live: HashMap<String, u8>,
    ops: Vec<Op>,
    w32: W32,
    i8w: Vec<i8>,
    high_water: usize,
}

impl<'g> Builder<'g> {
    fn alloc(&mut self) -> Result<u8, String> {
        let taken: HashSet<u8> = self.live.values().copied().collect();
        let s = (0..MAX_SLOTS as u8)
            .find(|s| !taken.contains(s))
            .ok_or("out of activation slots")?;
        self.high_water = self.high_water.max(s as usize + 1);
        Ok(s)
    }

    fn slot(&self, t: &str) -> Result<u8, String> {
        self.live
            .get(t)
            .copied()
            .ok_or_else(|| format!("tensor {t} is not live"))
    }

    /// One consumer of `t` is done; free its slot when it was the last.
    fn release(&mut self, t: &str) {
        if let Some(n) = self.uses.get_mut(t) {
            *n -= 1;
            if *n == 0 {
                self.live.remove(t);
            }
        }
    }

    fn conv(&self, node: &'g Node) -> Result<Conv<'g>, String> {
        let w = self
            .g
            .init
            .get(&node.inputs[1])
            .ok_or("conv weight not constant")?;
        let k = w.dims[2] as usize;
        let stride = node.ints("strides").first().copied().unwrap_or(1) as usize;
        let pads = node.ints("pads");
        let dil = node.ints("dilations");
        if w.dims[3] as usize != k
            || node.int("group").unwrap_or(1) != 1
            || dil.iter().any(|&d| d != 1)
        {
            return Err(format!("{}: unsupported conv geometry", node.outputs[0]));
        }
        if pads.iter().any(|&p| p as usize != k / 2)
            || node.ints("strides").iter().any(|&s| s as usize != stride)
        {
            return Err(format!("{}: unsupported padding/stride", node.outputs[0]));
        }
        Ok(Conv { node, k, stride })
    }

    fn pack_i8(&mut self, w: &[f32], cout: usize, cin: usize, k: usize) -> (u32, u32) {
        pack_i8_into(&mut self.i8w, &mut self.w32, w, cout, cin, k)
    }

    /// Pack `[cout][cin][k][k]` to the engine's `[cout/8][k*k][cin][8]` and emit.
    fn emit_conv(&mut self, c: &Conv, dst: u8, flags: u8) -> Result<(), String> {
        let src = self.slot(&c.node.inputs[0])?;
        let w = &self.g.init[&c.node.inputs[1]];
        let bias = match c.node.inputs.get(2) {
            Some(b) => self.g.init[b].f32s(),
            None => vec![0.0; w.dims[0] as usize],
        };
        let (cout, cin, k) = (w.dims[0] as usize, w.dims[1] as usize, c.k);
        if cout % CO_BLOCK != 0 {
            return Err(format!(
                "{}: cout {cout} not a multiple of {CO_BLOCK}",
                c.node.outputs[0]
            ));
        }
        let src_w = w.f32s();
        let mut packed = vec![0f32; src_w.len()];
        for co in 0..cout {
            for ci in 0..cin {
                for tap in 0..k * k {
                    let (cb, j) = (co / CO_BLOCK, co % CO_BLOCK);
                    packed[((cb * k * k + tap) * cin + ci) * CO_BLOCK + j] =
                        src_w[(co * cin + ci) * k * k + tap];
                }
            }
        }
        let wo = self.w32.push(&packed);
        let bo = self.w32.push(&bias);
        let (w8, s8) = if cin % I8_CI_STEP == 0 {
            self.pack_i8(&src_w, cout, cin, k)
        } else {
            (NO_I8, 0)
        };
        self.ops.push(Op {
            kind: OpKind::Conv2d,
            flags,
            k: k as u8,
            stride: c.stride as u8,
            src,
            dst,
            cin: cin as u16,
            cout: cout as u16,
            w: wo,
            b: bo,
            aux: 0,
            w8,
            s8,
        });
        self.release(&c.node.inputs[0]);
        Ok(())
    }
}

pub fn convert(g: &Graph, source_sha256: [u8; 32]) -> Result<Vmw, String> {
    let mut consumers: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut uses: HashMap<&str, usize> = HashMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        for inp in &n.inputs {
            if !g.init.contains_key(inp) {
                consumers.entry(inp.as_str()).or_default().push(i);
                *uses.entry(inp.as_str()).or_default() += 1;
            }
        }
    }
    let only = |t: &str| -> Option<&Node> {
        match consumers.get(t).map(|v| v.as_slice()) {
            Some([i]) => Some(&g.nodes[*i]),
            _ => None,
        }
    };

    // feats [B, T, 80] -> Transpose -> Unsqueeze -> [B, 1, 80, T] is exactly the engine's input
    // slot (H = 80 frequency rows, W = T, C = 1).
    let feats = g.inputs.first().ok_or("no graph input")?;
    let tr = only(feats)
        .filter(|n| n.op == "Transpose" && n.ints("perm") == [0, 2, 1])
        .ok_or("expected Transpose(0,2,1) on the input")?;
    let un = only(&tr.outputs[0])
        .filter(|n| n.op == "Unsqueeze")
        .ok_or("expected Unsqueeze after Transpose")?;

    let mut b = Builder {
        g,
        uses,
        live: HashMap::new(),
        ops: Vec::new(),
        w32: W32::default(),
        i8w: Vec::new(),
        high_water: 1,
    };
    b.live.insert(un.outputs[0].clone(), 0);

    let mut pending: HashMap<&str, Conv> = HashMap::new();
    let mut fused_relu: HashSet<*const Node> = HashSet::new();
    let mut pooled = false;
    let mut emb_dim = 0usize;

    for n in &g.nodes {
        if std::ptr::eq(n, tr) || std::ptr::eq(n, un) {
            continue;
        }
        match n.op.as_str() {
            "Conv" if !pooled => {
                let c = b.conv(n)?;
                match only(&n.outputs[0]) {
                    Some(r) if r.op == "Relu" => {
                        let dst = b.alloc()?;
                        b.emit_conv(&c, dst, FLAG_RELU)?;
                        b.live.insert(r.outputs[0].clone(), dst);
                        fused_relu.insert(r as *const Node);
                    }
                    Some(a) if a.op == "Add" => {
                        pending.insert(n.outputs[0].as_str(), c);
                    }
                    _ => return Err(format!("{}: conv feeds neither Relu nor Add", n.outputs[0])),
                }
            }
            "Relu" if !pooled => {
                if !fused_relu.contains(&(n as *const Node)) {
                    return Err(format!("{}: unfused Relu", n.outputs[0]));
                }
            }
            "Add" if !pooled => {
                let (x, y) = (n.inputs[0].as_str(), n.inputs[1].as_str());
                let (main, target) = match (pending.remove(x), pending.remove(y)) {
                    (Some(m), None) | (None, Some(m)) => {
                        // Identity shortcut: the block input's slot becomes the output, so it must
                        // have no other readers left.
                        let r = if m.node.outputs[0] == x { y } else { x };
                        let s = b.slot(r)?;
                        if b.uses.get(r) != Some(&1) {
                            return Err(format!("{r}: residual still has readers"));
                        }
                        b.uses.insert(r, 0);
                        b.live.remove(r);
                        (m, s)
                    }
                    (Some(p), Some(q)) => {
                        let (short, main) = if p.k == 1 { (p, q) } else { (q, p) };
                        let s = b.alloc()?;
                        b.emit_conv(&short, s, 0)?;
                        (main, s)
                    }
                    (None, None) => {
                        return Err(format!("{}: Add of two plain tensors", n.outputs[0]))
                    }
                };
                let mut flags = FLAG_ACCUM;
                let mut out = n.outputs[0].clone();
                if let Some(r) = only(&n.outputs[0]).filter(|r| r.op == "Relu") {
                    flags |= FLAG_RELU;
                    out = r.outputs[0].clone();
                    fused_relu.insert(r as *const Node);
                }
                b.emit_conv(&main, target, flags)?;
                b.live.insert(out, target);
            }
            "ReduceMean" if !pooled => {
                pooled = true;
                let src = b.slot(&n.inputs[0])?;
                let eps = pool_epsilon(g)?;
                b.ops.push(Op {
                    kind: OpKind::TstpPool,
                    flags: 0,
                    k: 0,
                    stride: 0,
                    src,
                    dst: 0,
                    cin: 0,
                    cout: 0,
                    w: 0,
                    b: 0,
                    aux: eps.to_bits(),
                    w8: NO_I8,
                    s8: 0,
                });
            }
            "Gemm" => {
                if n.int("transB") != Some(1)
                    || n.attrs.get("alpha").and_then(|a| a.f).unwrap_or(1.0) != 1.0
                {
                    return Err("Gemm must be transB=1, alpha=1".into());
                }
                let w = &g.init[&n.inputs[1]];
                let mut bias = g.init[&n.inputs[2]].f32s();
                let (cout, cin) = (w.dims[0] as usize, w.dims[1] as usize);
                // A trailing `Sub(gemm, mean_vec)` (the LM models' mean embedding) folds into the bias.
                if let Some(s) = only(&n.outputs[0]).filter(|s| s.op == "Sub") {
                    let mean = g
                        .init
                        .get(&s.inputs[1])
                        .ok_or("Sub after Gemm is not by a constant")?
                        .f32s();
                    for (bb, m) in bias.iter_mut().zip(mean) {
                        *bb -= m;
                    }
                }
                let wo = b.w32.push(&w.f32s());
                let bo = b.w32.push(&bias);
                b.ops.push(Op {
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
                emb_dim = cout;
            }
            // The pooling tail's elementwise arithmetic is what TstpPool computes.
            _ if pooled => {}
            other => return Err(format!("unsupported op {other} ({})", n.outputs[0])),
        }
    }
    if emb_dim == 0 || !pending.is_empty() {
        return Err("graph did not end in pooling + Gemm".into());
    }
    Ok(Vmw {
        arch: Arch::ResNet,
        frontend: 0,
        emb_dim,
        n_slots: b.high_water,
        feat_dim: 80,
        source_sha256,
        ops: b.ops,
        w32: b.w32.data,
        i8w: b.i8w,
        extra: Vec::new(),
    })
}

/// The epsilon added to the variance before the square root: `Sqrt(Add(v, eps))`.
fn pool_epsilon(g: &Graph) -> Result<f32, String> {
    let sqrt = g
        .nodes
        .iter()
        .find(|n| n.op == "Sqrt")
        .ok_or("pooling has no Sqrt")?;
    let add = g
        .nodes
        .iter()
        .find(|n| n.outputs[0] == sqrt.inputs[0] && n.op == "Add")
        .ok_or("Sqrt input is not an Add")?;
    let c = add
        .inputs
        .iter()
        .find_map(|i| g.init.get(i))
        .ok_or("no epsilon constant")?;
    Ok(c.data[0] as f32)
}

/// Symmetric int8 per output channel, packed `[cout/8][k*k][cin/4][8 co][4 ci]` so one 16-byte load
/// is 4 x 4 for SDOT. Returns the `i8w` offset and the `w32` index of the scales, then the sums.
pub(crate) fn pack_i8_into(
    i8w: &mut Vec<i8>,
    w32: &mut W32,
    w: &[f32],
    cout: usize,
    cin: usize,
    k: usize,
) -> (u32, u32) {
    let taps = k * k;
    let scales: Vec<f32> = (0..cout)
        .map(|co| {
            let m = w[co * cin * taps..(co + 1) * cin * taps]
                .iter()
                .fold(0f32, |m, v| m.max(v.abs()));
            if m > 0.0 {
                m / 127.0
            } else {
                1.0
            }
        })
        .collect();
    while !i8w.len().is_multiple_of(64) {
        i8w.push(0);
    }
    let at = i8w.len();
    let mut packed = vec![0i8; cout * cin * taps];
    let mut sums = vec![0f32; cout];
    for co in 0..cout {
        for ci in 0..cin {
            for tap in 0..taps {
                let q = (w[(co * cin + ci) * taps + tap] / scales[co])
                    .round()
                    .clamp(-127.0, 127.0);
                sums[co] += q;
                let (cb, j) = (co / CO_BLOCK, co % CO_BLOCK);
                let (g, kk) = (ci / 4, ci % 4);
                packed[(((cb * taps + tap) * (cin / 4) + g) * CO_BLOCK + j) * 4 + kk] = q as i8;
            }
        }
    }
    i8w.extend_from_slice(&packed);
    let s8 = w32.push(&scales);
    w32.data.extend_from_slice(&sums);
    (at as u32, s8)
}
