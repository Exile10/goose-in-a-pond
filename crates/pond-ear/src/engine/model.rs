//! `.vmw`, the engine's weights file, read in place: the converter does every layout transform
//! offline, so loading is a bounds check and a pointer cast. The layout is in docs/developer/pond-ear.md.

pub const MAGIC: &[u8; 4] = b"VMW1";
pub const VERSION: u32 = 1;
pub const HEADER: usize = 72;
pub const SECTION_ENTRY: usize = 16;
pub const OP_SIZE: usize = 32;
/// Output channels per packed weight block: two NEON f32 vectors.
pub const CO_BLOCK: usize = 8;
/// Activation slots a program may use (slot 0 is the input features).
pub const MAX_SLOTS: usize = 8;
/// `Op::w8` when a conv has no int8 weights (the stem: one input channel).
pub const NO_I8: u32 = u32::MAX;
/// Input channels per SDOT step: four groups of four.
pub const I8_CI_STEP: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    ResNet = 1,
    CamPlus = 2,
    /// A wake-word detector: depthwise-separable CNN, one logit out.
    Kws = 3,
    /// Silero VAD v5, 16 kHz: no op program, its weights in `silero::LAYOUT` order.
    Vad = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum OpKind {
    /// 2-D convolution over a channels-last activation with a 1-pixel zero halo; weights packed
    /// `[cout/8][k*k taps][cin][8]`, bias `[cout]`.
    Conv2d = 1,
    /// Temporal statistics pooling: per (channel, freq) mean and unbiased std over time, means then
    /// stds; `aux` is the f32 bits of the variance epsilon.
    TstpPool = 2,
    /// Dense layer on the pooled vector. `w32[w..]` row-major `[cout][cin]`.
    Linear = 3,
    /// Depthwise k x k convolution (cin == cout), weights packed `[k*k taps][c]`.
    Depthwise = 4,
    /// Global average pooling over frequency and time: `[c]` into the pool region.
    GapPool = 5,
}

pub const FLAG_RELU: u8 = 1;
/// The destination already holds a residual of the output's shape: add into it.
pub const FLAG_ACCUM: u8 = 2;

/// One instruction of a model's program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Op {
    pub kind: OpKind,
    pub flags: u8,
    pub k: u8,
    pub stride: u8,
    pub src: u8,
    pub dst: u8,
    pub cin: u16,
    pub cout: u16,
    pub w: u32,
    pub b: u32,
    pub aux: u32,
    /// Byte offset of int8 weights in "i8w", packed `[cout/8][k*k][cin/4][8][4]`, or `NO_I8`.
    pub w8: u32,
    /// `w32` index of the per-output-channel int8 scales, followed by the int8 weight sums.
    pub s8: u32,
}

impl Op {
    pub fn relu(&self) -> bool {
        self.flags & FLAG_RELU != 0
    }
    pub fn accum(&self) -> bool {
        self.flags & FLAG_ACCUM != 0
    }
    pub fn pad(&self) -> usize {
        self.k as usize / 2
    }

    pub fn encode(&self) -> [u8; OP_SIZE] {
        let mut r = [0u8; OP_SIZE];
        r[0] = self.kind as u8;
        r[1] = self.flags;
        r[2] = self.k;
        r[3] = self.stride;
        r[4] = self.src;
        r[5] = self.dst;
        r[8..10].copy_from_slice(&self.cin.to_le_bytes());
        r[10..12].copy_from_slice(&self.cout.to_le_bytes());
        r[12..16].copy_from_slice(&self.w.to_le_bytes());
        r[16..20].copy_from_slice(&self.b.to_le_bytes());
        r[20..24].copy_from_slice(&self.aux.to_le_bytes());
        r[24..28].copy_from_slice(&self.w8.to_le_bytes());
        r[28..32].copy_from_slice(&self.s8.to_le_bytes());
        r
    }

    fn decode(r: &[u8]) -> Result<Op, Error> {
        let kind = match r[0] {
            1 => OpKind::Conv2d,
            2 => OpKind::TstpPool,
            3 => OpKind::Linear,
            4 => OpKind::Depthwise,
            5 => OpKind::GapPool,
            _ => return Err(Error::BadOp),
        };
        Ok(Op {
            kind,
            flags: r[1],
            k: r[2],
            stride: r[3],
            src: r[4],
            dst: r[5],
            cin: u16::from_le_bytes([r[8], r[9]]),
            cout: u16::from_le_bytes([r[10], r[11]]),
            w: u32_at(r, 12),
            b: u32_at(r, 16),
            aux: u32_at(r, 20),
            w8: u32_at(r, 24),
            s8: u32_at(r, 28),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Truncated,
    BadMagic,
    BadVersion,
    BadOp,
    MissingSection,
    Misaligned,
}

/// A named section of a `.vmw`, for sections beyond the op program and weights.
pub fn section<'a>(bytes: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    if bytes.len() < HEADER || name.len() > 8 {
        return None;
    }
    for i in 0..u32_at(bytes, 8) as usize {
        let e = HEADER + i * SECTION_ENTRY;
        let n = bytes.get(e..e + 8)?;
        if &n[..name.len()] == name && n[name.len()..].iter().all(|&c| c == 0) {
            let (off, size) = (
                u32_at(bytes, e + 8) as usize,
                u32_at(bytes, e + 12) as usize,
            );
            return bytes.get(off..off + size);
        }
    }
    None
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A loaded model: a view over the file's bytes, which the caller keeps alive.
#[derive(Clone, Copy)]
pub struct Model<'a> {
    pub arch: Arch,
    /// 0 = WeSpeaker front end, 1 = 3D-Speaker (see `fbank`).
    pub frontend: u32,
    pub emb_dim: usize,
    pub n_slots: usize,
    pub feat_dim: usize,
    pub source_sha256: [u8; 32],
    ops: &'a [u8],
    pub w32: &'a [f32],
    /// Empty when the file carries no int8 weights.
    pub w8: &'a [i8],
}

impl<'a> Model<'a> {
    /// Parse and validate. `bytes` must be 4-byte aligned (`convert::Aligned`); weights are then
    /// used in place.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER {
            return Err(Error::Truncated);
        }
        if &bytes[0..4] != MAGIC {
            return Err(Error::BadMagic);
        }
        if u32_at(bytes, 4) != VERSION {
            return Err(Error::BadVersion);
        }
        let n_sections = u32_at(bytes, 8) as usize;
        let n_ops = u32_at(bytes, 12) as usize;
        let arch = match u32_at(bytes, 16) {
            1 => Arch::ResNet,
            2 => Arch::CamPlus,
            3 => Arch::Kws,
            4 => Arch::Vad,
            _ => return Err(Error::BadOp),
        };
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&bytes[40..72]);

        let section = |name: &[u8]| -> Result<&'a [u8], Error> {
            for i in 0..n_sections {
                let e = HEADER + i * SECTION_ENTRY;
                if e + SECTION_ENTRY > bytes.len() {
                    return Err(Error::Truncated);
                }
                let n = &bytes[e..e + 8];
                if &n[..name.len()] == name && n[name.len()..].iter().all(|&c| c == 0) {
                    let (off, size) = (
                        u32_at(bytes, e + 8) as usize,
                        u32_at(bytes, e + 12) as usize,
                    );
                    return bytes.get(off..off + size).ok_or(Error::Truncated);
                }
            }
            Err(Error::MissingSection)
        };

        let ops = section(b"ops")?;
        if ops.len() < n_ops * OP_SIZE {
            return Err(Error::Truncated);
        }
        let w = section(b"w32")?;
        if !(w.as_ptr() as usize).is_multiple_of(4) {
            return Err(Error::Misaligned);
        }
        // SAFETY: alignment checked above; any bit pattern is a valid f32; the slice borrows
        // `bytes`, which outlives `Model<'a>`.
        let w32 = unsafe { core::slice::from_raw_parts(w.as_ptr() as *const f32, w.len() / 4) };
        let w8 = match section(b"i8w") {
            // SAFETY: i8 and u8 share size and alignment; any byte is a valid i8.
            Ok(b) => unsafe { core::slice::from_raw_parts(b.as_ptr() as *const i8, b.len()) },
            Err(Error::MissingSection) => &[],
            Err(e) => return Err(e),
        };

        let m = Model {
            arch,
            frontend: u32_at(bytes, 20),
            emb_dim: u32_at(bytes, 24) as usize,
            n_slots: u32_at(bytes, 28) as usize,
            feat_dim: u32_at(bytes, 32) as usize,
            source_sha256: sha,
            ops: &ops[..n_ops * OP_SIZE],
            w32,
            w8,
        };
        m.validate()?;
        Ok(m)
    }

    pub fn n_ops(&self) -> usize {
        self.ops.len() / OP_SIZE
    }

    /// Op `i`. Every record was decoded once in `validate`, so this cannot fail.
    pub fn op(&self, i: usize) -> Op {
        match Op::decode(&self.ops[i * OP_SIZE..(i + 1) * OP_SIZE]) {
            Ok(op) => op,
            Err(_) => unreachable!("validated at parse"),
        }
    }

    /// Every op decodes, every weight range is inside `w32`, every slot exists.
    fn validate(&self) -> Result<(), Error> {
        if self.n_slots == 0 || self.n_slots > MAX_SLOTS {
            return Err(Error::BadOp);
        }
        for i in 0..self.n_ops() {
            let op = Op::decode(&self.ops[i * OP_SIZE..(i + 1) * OP_SIZE])?;
            let (wn, bn) = match op.kind {
                OpKind::Conv2d => {
                    let k = op.k as usize;
                    (op.cout as usize * k * k * op.cin as usize, op.cout as usize)
                }
                OpKind::Linear => (op.cout as usize * op.cin as usize, op.cout as usize),
                OpKind::Depthwise => {
                    let k = op.k as usize;
                    (k * k * op.cout as usize, op.cout as usize)
                }
                OpKind::TstpPool | OpKind::GapPool => (0, 0),
            };
            if op.w as usize + wn > self.w32.len() || op.b as usize + bn > self.w32.len() {
                return Err(Error::Truncated);
            }
            if op.kind == OpKind::Conv2d
                && (!(op.cout as usize).is_multiple_of(CO_BLOCK)
                    || op.stride == 0
                    || op.src == op.dst)
            {
                return Err(Error::BadOp);
            }
            if op.kind == OpKind::Depthwise
                && (op.cin != op.cout
                    || !op.cout.is_multiple_of(4)
                    || op.stride == 0
                    || op.src == op.dst)
            {
                return Err(Error::BadOp);
            }
            if op.src as usize >= self.n_slots || op.dst as usize >= self.n_slots {
                return Err(Error::BadOp);
            }
            if op.kind == OpKind::Conv2d
                && op.w8 != NO_I8
                && (!(op.cin as usize).is_multiple_of(I8_CI_STEP)
                    || op.w8 as usize + wn > self.w8.len()
                    || op.s8 as usize + 2 * op.cout as usize > self.w32.len())
            {
                return Err(Error::BadOp);
            }
        }
        Ok(())
    }
}
