//! Writing `.vmw` files; the reader is `engine::model`.

use crate::engine::model::{Arch, Op, HEADER, MAGIC, OP_SIZE, SECTION_ENTRY, VERSION};

/// Bumped whenever the converter's output changes, so a cached `.vmw` from an older converter is
/// rebuilt rather than silently reused (header word 9).
pub const CONVERTER: u32 = 5;

/// The f32 weight section under construction; every tensor starts on a 64-byte boundary so kernels
/// can assume aligned blocks.
#[derive(Default)]
pub struct W32 {
    pub data: Vec<f32>,
}

impl W32 {
    pub fn push(&mut self, v: &[f32]) -> u32 {
        while !self.data.len().is_multiple_of(16) {
            self.data.push(0.0);
        }
        let at = self.data.len() as u32;
        self.data.extend_from_slice(v);
        at
    }
}

pub struct Vmw {
    pub arch: Arch,
    pub frontend: u32,
    pub emb_dim: usize,
    pub n_slots: usize,
    pub feat_dim: usize,
    pub source_sha256: [u8; 32],
    pub ops: Vec<Op>,
    pub w32: Vec<f32>,
    pub i8w: Vec<i8>,
    /// Further named sections, at most 8-byte names.
    pub extra: Vec<([u8; 8], Vec<u8>)>,
}

fn align64(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(64) {
        v.push(0);
    }
}

impl Vmw {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut sections: Vec<(&[u8], Vec<u8>)> = vec![
            (b"ops", self.ops.iter().flat_map(|o| o.encode()).collect()),
            (
                b"w32",
                self.w32.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ),
        ];
        if !self.i8w.is_empty() {
            sections.push((b"i8w", self.i8w.iter().map(|&v| v as u8).collect()));
        }
        for (name, body) in &self.extra {
            let len = name.iter().position(|&c| c == 0).unwrap_or(8);
            sections.push((&name[..len], body.clone()));
        }
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        for v in [
            VERSION,
            sections.len() as u32,
            self.ops.len() as u32,
            self.arch as u32,
            self.frontend,
            self.emb_dim as u32,
            self.n_slots as u32,
            self.feat_dim as u32,
            CONVERTER,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.source_sha256);
        assert_eq!(out.len(), HEADER);

        let dir_at = out.len();
        out.resize(dir_at + sections.len() * SECTION_ENTRY, 0);
        for (i, (name, body)) in sections.iter().enumerate() {
            align64(&mut out);
            let off = out.len();
            out.extend_from_slice(body);
            let e = dir_at + i * SECTION_ENTRY;
            out[e..e + name.len()].copy_from_slice(name);
            out[e + 8..e + 12].copy_from_slice(&(off as u32).to_le_bytes());
            out[e + 12..e + 16].copy_from_slice(&(body.len() as u32).to_le_bytes());
        }
        debug_assert_eq!(sections[0].1.len(), self.ops.len() * OP_SIZE);
        out
    }
}

/// File bytes in u32-aligned storage, so `Model::parse` can view weights in place.
pub struct Aligned {
    words: Vec<u32>,
    len: usize,
}

impl Aligned {
    pub fn new(bytes: &[u8]) -> Self {
        let mut words = vec![0u32; bytes.len().div_ceil(4)];
        // SAFETY: `words` has at least `bytes.len()` bytes; u32 has no invalid patterns.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                words.as_mut_ptr() as *mut u8,
                bytes.len(),
            )
        };
        Aligned {
            words,
            len: bytes.len(),
        }
    }

    /// Read a file straight into aligned storage: one buffer, so a 32 MB model peaks at 32 MB, not 64.
    pub fn read(path: &std::path::Path) -> std::io::Result<Self> {
        use std::io::Read;
        let mut f = std::fs::File::open(path)?;
        let len = f.metadata()?.len() as usize;
        let mut words = vec![0u32; len.div_ceil(4)];
        // SAFETY: `words` owns at least `len` bytes, and any byte pattern is a valid u32.
        let buf = unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr() as *mut u8, len) };
        f.read_exact(buf)?;
        Ok(Aligned { words, len })
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the Vec owns `len` initialised bytes; lifetime tied to &self.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr() as *const u8, self.len) }
    }
}
