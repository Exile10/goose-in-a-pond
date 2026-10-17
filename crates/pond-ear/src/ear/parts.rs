//! The parts: separate small models reading the shared front end, each owning its arena.

use std::time::Instant;

use crate::convert::ModelFile;
use crate::engine::exec::{self, Plan};
use crate::engine::fbank::{self, FRAME, NUM_BINS, SHIFT};
use crate::engine::kernels::Rung;
use crate::engine::model::{Arch, Model, OpKind, NO_I8};
use crate::engine::silero;
use crate::wake::detector::SEGMENT_PAD;
use crate::wake::{window_cmn, HOP_FRAMES, WINDOW_FRAMES, WINDOW_SAMPLES};
use crate::Error;

use super::front::Front;
use super::Bytes;

/// Bytes of a `.vmw` that `rung` reads: an int8 rung skips the f32 copies of every conv with int8
/// weights, the f32 rungs skip the int8 section. What a per-rung file would hold.
pub fn rung_bytes(bytes: &[u8], rung: Rung) -> usize {
    let Ok(m) = Model::parse(bytes) else {
        return bytes.len();
    };
    let int8 = is_int8(rung);
    let mut unused = if int8 { 0 } else { std::mem::size_of_val(m.w8) };
    for i in 0..m.n_ops() {
        let op = m.op(i);
        if op.kind == OpKind::Conv2d && op.w8 != NO_I8 {
            let k = op.k as usize;
            unused += if int8 {
                op.cout as usize * k * k * op.cin as usize * 4
            } else {
                2 * op.cout as usize * 4
            };
        }
    }
    bytes.len() - unused.min(bytes.len())
}

fn is_int8(rung: Rung) -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        rung == Rung::I8Sdot
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = rung;
        false
    }
}

/// The wake CNN, scored every 80 ms on the last 1.2 s of shared frames: the same arithmetic as
/// `wake::detector::Detector`, without its own audio and frames.
pub struct WakeCnn {
    pub label: String,
    file: ModelFile,
    plan: Plan,
    arena: Vec<f32>,
    window: Vec<f32>,
    seg: Vec<f32>,
    pub threshold: f32,
    pub cooldown_s: f32,
    cooldown_until: usize,
    pub ns: u64,
    pub calls: u64,
}

pub struct Hop {
    pub t: f64,
    pub p: f32,
    /// The wake segment (absolute samples) when this hop is a trigger.
    pub trigger: Option<(usize, usize)>,
}

impl WakeCnn {
    pub fn new(file: ModelFile, label: &str, threshold: f32) -> Result<WakeCnn, Error> {
        let model = file.model();
        if model.arch != Arch::Kws || model.emb_dim != 1 {
            return Err(Error::WrongModel(format!("{label}: not a wake-word CNN")));
        }
        let plan = exec::plan(&model, WINDOW_FRAMES).map_err(Error::Plan)?;
        Ok(WakeCnn {
            label: label.to_string(),
            arena: vec![0f32; plan.arena],
            window: vec![0f32; WINDOW_FRAMES * NUM_BINS],
            seg: Vec::with_capacity(WINDOW_SAMPLES),
            file,
            plan,
            threshold,
            cooldown_s: 2.0,
            cooldown_until: 0,
            ns: 0,
            calls: 0,
        })
    }

    pub fn reset(&mut self) {
        self.cooldown_until = 0;
        self.ns = 0;
        self.calls = 0;
    }

    pub fn macs(&self) -> u64 {
        self.plan.macs
    }

    pub fn bytes(&self, rung: Rung) -> Bytes {
        Bytes {
            weights: self.file.bytes().len(),
            weights_rung: rung_bytes(self.file.bytes(), rung),
            arena: self.plan.arena * 4,
            state: (self.window.len() + self.seg.capacity()) * 4,
        }
    }

    fn score(&mut self, rung: Rung) -> f32 {
        let mut out = [0f32; 1];
        let t = Instant::now();
        // Planned for this model and window when loaded, so it cannot fail.
        let _ = exec::run(
            &self.file.model(),
            &self.plan,
            rung,
            &self.window,
            &mut self.arena,
            &mut out,
            None,
        );
        self.ns += t.elapsed().as_nanos() as u64;
        self.calls += 1;
        crate::wake::sigmoid(out[0])
    }

    /// Score once on silence, so the arena is resident before RAM is measured.
    pub fn warm_up(&mut self, rung: Rung) {
        self.window.iter_mut().for_each(|v| *v = 0.0);
        self.score(rung);
        self.ns = 0;
        self.calls = 0;
    }

    /// Called as frame count `n` becomes available; scores when a hop is due.
    pub fn on_frame(&mut self, front: &Front, n: usize, rung: Rung) -> Option<Hop> {
        if n < WINDOW_FRAMES || !(n - WINDOW_FRAMES).is_multiple_of(HOP_FRAMES) {
            return None;
        }
        let got = front.frames(n - WINDOW_FRAMES, n, &mut self.window);
        debug_assert_eq!(got, WINDOW_FRAMES);
        window_cmn(&mut self.window);
        let p = self.score(rung);
        let end = (n - 1) * SHIFT + FRAME;
        let t = end as f64 / 16_000.0;
        let mut trigger = None;
        if p >= self.threshold && n >= self.cooldown_until {
            let start = end.saturating_sub(WINDOW_SAMPLES);
            front.audio(start, end, &mut self.seg);
            let q = crate::dsp::quality(&self.seg);
            let (s, e) = if q.speech_end > q.speech_start {
                (start + q.speech_start, start + q.speech_end)
            } else {
                (start, end)
            };
            trigger = Some((
                s.saturating_sub(SEGMENT_PAD).max(start),
                (e + SEGMENT_PAD).min(end),
            ));
            self.cooldown_until = n + (self.cooldown_s * 100.0) as usize;
        }
        Some(Hop { t, p, trigger })
    }
}

/// Voice Match's speaker network (ResNet34) on a span of the shared frames.
pub struct SpeakerNet {
    pub label: String,
    file: ModelFile,
    pub max_frames: usize,
    arena: Vec<f32>,
    feats: Vec<f32>,
    emb: Vec<f32>,
    pub ns: u64,
    pub calls: u64,
    pub last_macs: u64,
}

impl SpeakerNet {
    /// `max_seconds` bounds the longest span, which fixes the arena once.
    pub fn new(file: ModelFile, label: &str, max_seconds: f32) -> Result<SpeakerNet, Error> {
        let model = file.model();
        if model.arch != Arch::ResNet {
            return Err(Error::WrongModel(format!(
                "{label}: not a ResNet speaker model"
            )));
        }
        if model.frontend != 0 {
            return Err(Error::WrongModel(format!(
                "{label}: the shared frames are WeSpeaker fbank; this model wants another front end"
            )));
        }
        let max_frames = (max_seconds * 100.0) as usize;
        let plan = exec::plan(&model, max_frames).map_err(Error::Plan)?;
        Ok(SpeakerNet {
            label: label.to_string(),
            arena: vec![0f32; plan.arena],
            feats: vec![0f32; max_frames * NUM_BINS],
            emb: vec![0f32; model.emb_dim],
            file,
            max_frames,
            ns: 0,
            calls: 0,
            last_macs: plan.macs,
        })
    }

    pub fn reset(&mut self) {
        self.ns = 0;
        self.calls = 0;
    }

    pub fn emb_dim(&self) -> usize {
        self.emb.len()
    }

    pub fn bytes(&self, rung: Rung) -> Bytes {
        Bytes {
            weights: self.file.bytes().len(),
            weights_rung: rung_bytes(self.file.bytes(), rung),
            arena: self.arena.len() * 4,
            state: (self.feats.len() + self.emb.len()) * 4,
        }
    }

    /// Embedding of frames `[start, end)`, mean-normalised over that span as Voice Match was
    /// trained; the newest `max_frames` when the span is longer.
    pub fn embed(
        &mut self,
        front: &Front,
        start: usize,
        end: usize,
        rung: Rung,
    ) -> Result<&[f32], Error> {
        let start = start.max(end.saturating_sub(self.max_frames));
        let n = front.frames(start, end, &mut self.feats);
        let model = self.file.model();
        let t = Instant::now();
        fbank::cmn(&mut self.feats, n);
        let plan = exec::plan(&model, n).map_err(Error::Plan)?;
        exec::run(
            &model,
            &plan,
            rung,
            &self.feats[..n * NUM_BINS],
            &mut self.arena[..plan.arena],
            &mut self.emb,
            None,
        )
        .map_err(Error::Plan)?;
        self.ns += t.elapsed().as_nanos() as u64;
        self.calls += 1;
        self.last_macs = plan.macs;
        Ok(&self.emb)
    }

    /// One full-length pass on silence, so the whole arena is resident.
    pub fn warm_up(&mut self, rung: Rung) {
        let model = self.file.model();
        self.feats.iter_mut().for_each(|v| *v = 0.0);
        if let Ok(plan) = exec::plan(&model, self.max_frames) {
            let _ = exec::run(
                &model,
                &plan,
                rung,
                &self.feats,
                &mut self.arena,
                &mut self.emb,
                None,
            );
        }
    }
}

/// Silero VAD v5 on the shared ring: one call per 512 new samples (32 ms), with the official
/// wrapper's 64-sample context.
pub struct VadSilero {
    pub label: String,
    file: ModelFile,
    state: silero::State,
    scratch: Box<silero::Scratch>,
    chunk: Vec<f32>,
    /// Absolute sample where the next chunk starts.
    pub next: usize,
    pub ns: u64,
    pub calls: u64,
}

impl VadSilero {
    pub fn new(file: ModelFile, label: &str) -> Result<VadSilero, Error> {
        let model = file.model();
        if model.arch != Arch::Vad || model.w32.len() < silero::offsets().1 {
            return Err(Error::WrongModel(format!(
                "{label}: not a Silero VAD v5 model"
            )));
        }
        Ok(VadSilero {
            label: label.to_string(),
            file,
            state: silero::State::new(),
            scratch: Box::new(silero::Scratch::new()),
            chunk: Vec::with_capacity(silero::CHUNK),
            next: 0,
            ns: 0,
            calls: 0,
        })
    }

    pub fn reset(&mut self) {
        self.state = silero::State::new();
        self.next = 0;
        self.ns = 0;
        self.calls = 0;
    }

    pub fn bytes(&self) -> Bytes {
        let w = self.file.bytes().len();
        Bytes {
            weights: w,
            weights_rung: w,
            arena: std::mem::size_of::<silero::Scratch>(),
            state: std::mem::size_of::<silero::State>() + self.chunk.capacity() * 4,
        }
    }

    /// Absolute sample at which the next chunk ends.
    pub fn next_end(&self) -> usize {
        self.next + silero::CHUNK
    }

    /// Run the next chunk if the ring holds it: `(end sample, speech probability)`.
    pub fn step(&mut self, front: &Front, rung: Rung) -> Option<(usize, f32)> {
        let end = self.next_end();
        if end > front.samples() {
            return None;
        }
        front.audio(self.next, end, &mut self.chunk);
        if self.chunk.len() != silero::CHUNK {
            // Fell out of the ring (a stall longer than it holds): restart cleanly.
            self.state = silero::State::new();
            self.next = front.samples().saturating_sub(silero::CHUNK);
            return None;
        }
        let model = self.file.model();
        let t = Instant::now();
        let p = silero::step(
            rung,
            model.w32,
            &mut self.state,
            &mut self.scratch,
            &self.chunk,
        );
        self.ns += t.elapsed().as_nanos() as u64;
        self.calls += 1;
        self.next = end;
        Some((end, p))
    }
}
