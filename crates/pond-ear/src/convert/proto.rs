//! Just enough protobuf to read an ONNX file: the wire format and the messages our models use.
//! Field numbers are from onnx/onnx.proto3; unknown fields are skipped, as protobuf requires.

use std::collections::HashMap;

pub enum Wire<'a> {
    Varint(u64),
    Fixed64(u64),
    Len(&'a [u8]),
    Fixed32(u32),
}

pub struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, i: 0 }
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.b.get(self.i).ok_or("truncated varint")?;
            self.i += 1;
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("varint too long".into())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self.b.get(self.i..self.i + n).ok_or("truncated field")?;
        self.i += n;
        Ok(s)
    }

    /// Next `(field number, value)`, or `None` at the end of the message.
    pub fn next_field(&mut self) -> Result<Option<(u32, Wire<'a>)>, String> {
        if self.i >= self.b.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let (field, wire) = ((key >> 3) as u32, key & 7);
        let v = match wire {
            0 => Wire::Varint(self.varint()?),
            1 => Wire::Fixed64(u64::from_le_bytes(le(self.take(8)?))),
            2 => {
                let n = self.varint()? as usize;
                Wire::Len(self.take(n)?)
            }
            5 => Wire::Fixed32(u32::from_le_bytes(le(self.take(4)?))),
            w => return Err(format!("unsupported wire type {w}")),
        };
        Ok(Some((field, v)))
    }
}

/// The first `K` bytes as an array; callers pass slices of at least that length.
fn le<const K: usize>(b: &[u8]) -> [u8; K] {
    let mut a = [0u8; K];
    a.copy_from_slice(&b[..K]);
    a
}

fn string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Repeated int64 that may arrive packed (one LEN) or unpacked (many varints).
fn push_ints(v: &mut Vec<i64>, w: Wire) -> Result<(), String> {
    match w {
        Wire::Varint(x) => v.push(x as i64),
        Wire::Len(b) => {
            let mut r = Reader::new(b);
            while r.i < b.len() {
                v.push(r.varint()? as i64);
            }
        }
        _ => return Err("bad int64 encoding".into()),
    }
    Ok(())
}

fn push_floats(v: &mut Vec<f32>, w: Wire) -> Result<(), String> {
    match w {
        Wire::Fixed32(x) => v.push(f32::from_bits(x)),
        Wire::Len(b) => v.extend(b.chunks_exact(4).map(|c| f32::from_le_bytes(le(c)))),
        _ => return Err("bad float encoding".into()),
    }
    Ok(())
}

/// TensorProto, its data widened to f64 so float and int64 constants share one representation
/// (f32 weights survive the round trip exactly).
#[derive(Clone, Debug, Default)]
pub struct Tensor {
    pub name: String,
    pub dims: Vec<i64>,
    pub data: Vec<f64>,
}

impl Tensor {
    fn parse(b: &[u8]) -> Result<Tensor, String> {
        let mut t = Tensor::default();
        let (mut dtype, mut raw, mut floats, mut ints) = (0i64, None, Vec::new(), Vec::new());
        let mut r = Reader::new(b);
        while let Some((f, w)) = r.next_field()? {
            match (f, w) {
                (1, w) => push_ints(&mut t.dims, w)?,
                (2, Wire::Varint(x)) => dtype = x as i64,
                (4, w) => push_floats(&mut floats, w)?,
                (7, w) => push_ints(&mut ints, w)?,
                (8, Wire::Len(s)) => t.name = string(s),
                (9, Wire::Len(s)) => raw = Some(s),
                _ => {}
            }
        }
        t.data = match (dtype, raw) {
            (1, Some(raw)) => raw
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(le(c)) as f64)
                .collect(),
            (7, Some(raw)) => raw
                .chunks_exact(8)
                .map(|c| i64::from_le_bytes(le(c)) as f64)
                .collect(),
            (1, None) => floats.into_iter().map(|x| x as f64).collect(),
            (7, None) => ints.into_iter().map(|x| x as f64).collect(),
            (d, _) => return Err(format!("tensor {}: unsupported data type {d}", t.name)),
        };
        Ok(t)
    }

    pub fn f32s(&self) -> Vec<f32> {
        self.data.iter().map(|&x| x as f32).collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Attr {
    pub f: Option<f32>,
    pub i: Option<i64>,
    pub ints: Vec<i64>,
    pub t: Option<Tensor>,
    /// A subgraph (the branches of `If`), parsed like the top graph.
    pub g: Option<Box<Graph>>,
}

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub op: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub attrs: HashMap<String, Attr>,
}

impl Node {
    fn parse(b: &[u8]) -> Result<Node, String> {
        let mut n = Node::default();
        let mut r = Reader::new(b);
        while let Some((f, w)) = r.next_field()? {
            match (f, w) {
                (1, Wire::Len(s)) => n.inputs.push(string(s)),
                (2, Wire::Len(s)) => n.outputs.push(string(s)),
                (4, Wire::Len(s)) => n.op = string(s),
                (5, Wire::Len(s)) => {
                    let (mut name, mut a) = (String::new(), Attr::default());
                    let mut ar = Reader::new(s);
                    while let Some((af, aw)) = ar.next_field()? {
                        match (af, aw) {
                            (1, Wire::Len(x)) => name = string(x),
                            (2, Wire::Fixed32(x)) => a.f = Some(f32::from_bits(x)),
                            (3, Wire::Varint(x)) => a.i = Some(x as i64),
                            (5, Wire::Len(x)) => a.t = Some(Tensor::parse(x)?),
                            (6, Wire::Len(x)) => a.g = Some(Box::new(parse_graph(x)?)),
                            (8, w) => push_ints(&mut a.ints, w)?,
                            _ => {}
                        }
                    }
                    n.attrs.insert(name, a);
                }
                _ => {}
            }
        }
        Ok(n)
    }

    pub fn ints(&self, name: &str) -> Vec<i64> {
        self.attrs
            .get(name)
            .map(|a| a.ints.clone())
            .unwrap_or_default()
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        self.attrs.get(name).and_then(|a| a.i)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub init: HashMap<String, Tensor>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

fn value_info_name(b: &[u8]) -> Result<String, String> {
    let mut r = Reader::new(b);
    while let Some((f, w)) = r.next_field()? {
        if let (1, Wire::Len(s)) = (f, w) {
            return Ok(string(s));
        }
    }
    Ok(String::new())
}

/// ModelProto -> its GraphProto, with Constant nodes folded into `init`.
pub fn parse_onnx(bytes: &[u8]) -> Result<Graph, String> {
    let mut r = Reader::new(bytes);
    let mut graph = None;
    while let Some((f, w)) = r.next_field()? {
        if let (7, Wire::Len(s)) = (f, w) {
            graph = Some(s);
        }
    }
    parse_graph(graph.ok_or("no graph in model")?)
}

/// One GraphProto (top level or a subgraph), Constant nodes folded into `init`.
pub fn parse_graph(gb: &[u8]) -> Result<Graph, String> {
    let mut g = Graph::default();
    let mut r = Reader::new(gb);
    while let Some((f, w)) = r.next_field()? {
        match (f, w) {
            (1, Wire::Len(s)) => g.nodes.push(Node::parse(s)?),
            (5, Wire::Len(s)) => {
                let t = Tensor::parse(s)?;
                g.init.insert(t.name.clone(), t);
            }
            (11, Wire::Len(s)) => g.inputs.push(value_info_name(s)?),
            (12, Wire::Len(s)) => g.outputs.push(value_info_name(s)?),
            _ => {}
        }
    }
    // Constants become initializers; the converter then sees one kind of constant.
    let mut nodes = Vec::with_capacity(g.nodes.len());
    for n in std::mem::take(&mut g.nodes) {
        if n.op == "Constant" {
            if let Some(t) = n.attrs.get("value").and_then(|a| a.t.clone()) {
                g.init.insert(
                    n.outputs[0].clone(),
                    Tensor {
                        name: n.outputs[0].clone(),
                        ..t
                    },
                );
                continue;
            }
        }
        nodes.push(n);
    }
    g.nodes = nodes;
    g.inputs.retain(|i| !g.init.contains_key(i));
    Ok(g)
}
