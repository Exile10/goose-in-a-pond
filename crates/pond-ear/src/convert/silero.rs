//! Silero VAD v5 ONNX -> `.vmw` (arch `Vad`): the 16 kHz branch's weights in the order the engine
//! reads them; every shape is checked, so a different model fails here, not with plausible numbers.

use crate::engine::model::Arch;
use crate::engine::silero::offsets;

use super::proto::{Graph, Node};
use super::vmw::{Vmw, W32};

fn weights(g: &Graph, name: &str, want: &[i64]) -> Result<Vec<f32>, String> {
    let t = g
        .init
        .get(name)
        .ok_or_else(|| format!("silero: missing weight {name}"))?;
    if t.dims != want {
        return Err(format!("silero: {name} is {:?}, expected {want:?}", t.dims));
    }
    Ok(t.f32s())
}

/// The 16 kHz branch, after checking the condition really is `sr == 16000`.
pub fn branch(g: &Graph) -> Result<&Graph, String> {
    let iff = g
        .nodes
        .iter()
        .find(|n| n.op == "If")
        .ok_or("silero: no If on the sample rate")?;
    let eq = g
        .nodes
        .iter()
        .find(|n| n.op == "Equal" && n.outputs.first() == iff.inputs.first())
        .ok_or("silero: If condition not found")?;
    let is16k = eq
        .inputs
        .iter()
        .any(|i| g.init.get(i).is_some_and(|t| t.data == [16_000.0]));
    if !is16k {
        return Err("silero: the If is not on sr == 16000".into());
    }
    iff.attrs
        .get("then_branch")
        .and_then(|a| a.g.as_deref())
        .ok_or_else(|| "silero: no then_branch".into())
}

pub fn convert(g: &Graph, sha: [u8; 32]) -> Result<Vmw, String> {
    let b = branch(g)?;
    let pad = b
        .nodes
        .iter()
        .find(|n| n.op == "Pad")
        .ok_or("silero: no Pad")?;
    let pads = pad
        .inputs
        .get(1)
        .and_then(|p| b.init.get(p))
        .map(|t| t.data.clone())
        .unwrap_or_default();
    if pads != [0.0, 0.0, 0.0, 64.0] {
        return Err(format!(
            "silero: expected a 64-sample right pad, got {pads:?}"
        ));
    }
    let convs: Vec<&Node> = b.nodes.iter().filter(|n| n.op == "Conv").collect();
    if convs.len() != 6 {
        return Err(format!("silero: expected 6 convs, found {}", convs.len()));
    }
    let lstm = b
        .nodes
        .iter()
        .find(|n| n.op == "LSTM")
        .ok_or("silero: no LSTM")?;
    if lstm.int("hidden_size") != Some(128) {
        return Err("silero: LSTM hidden size is not 128".into());
    }
    let mut w = W32::default();
    let mut put = |v: Vec<f32>| w.push(&v) as usize;
    let mut at = Vec::new();
    // STFT basis: no bias, stride 128.
    if convs[0].ints("strides") != [128] {
        return Err("silero: STFT conv stride is not 128".into());
    }
    at.push(put(weights(b, &convs[0].inputs[1], &[258, 1, 256])?));
    for (i, (cout, cin, stride)) in [(128, 129, 1), (64, 128, 2), (64, 64, 2), (128, 64, 1)]
        .into_iter()
        .enumerate()
    {
        let c = convs[i + 1];
        if c.ints("strides") != [stride] || c.ints("pads") != [1, 1] {
            return Err(format!(
                "silero: conv {} has stride {:?} pads {:?}",
                i + 1,
                c.ints("strides"),
                c.ints("pads")
            ));
        }
        at.push(put(weights(b, &c.inputs[1], &[cout, cin, 3])?));
        at.push(put(weights(b, &c.inputs[2], &[cout])?));
    }
    at.push(put(weights(b, &lstm.inputs[1], &[1, 512, 128])?));
    at.push(put(weights(b, &lstm.inputs[2], &[1, 512, 128])?));
    at.push(put(weights(b, &lstm.inputs[3], &[1, 1024])?));
    let out = convs[5];
    at.push(put(weights(b, &out.inputs[1], &[1, 128, 1])?));
    at.push(put(weights(b, &out.inputs[2], &[1])?));

    let (want, total) = offsets();
    if at != want || w.data.len() != total {
        return Err("silero: weight layout does not match the runtime's".into());
    }
    Ok(Vmw {
        arch: Arch::Vad,
        frontend: 2,
        emb_dim: 1,
        n_slots: 1,
        feat_dim: 0,
        source_sha256: sha,
        ops: Vec::new(),
        w32: w.data,
        i8w: Vec::new(),
        extra: Vec::new(),
    })
}
