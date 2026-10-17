//! The engine: speaker embeddings, wake scores and voice activity from 16 kHz PCM, hand-written to
//! the metal. Nothing here allocates: the caller owns one arena and passes it in.

pub mod exec;
pub mod fbank;
pub mod fft;
pub mod kernels;
pub mod math;
pub mod model;
pub mod silero;

#[allow(clippy::all)]
mod tables {
    include!(concat!(env!("OUT_DIR"), "/tables.rs"));
}
