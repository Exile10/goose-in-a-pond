//! The ear: wake word, voice activity, Voice Match and echo cancellation on one audio front end,
//! hand-written with std only. Map and measurements in docs/developer/pond-ear.md.

pub mod convert;
pub mod dsp;
pub mod ear;
pub mod engine;
pub mod locations;
pub mod score;
pub mod wake;
pub mod wav;

mod error;

pub use convert::ModelFile;
pub use ear::{Config, Ear, Event, Household, State};
pub use engine::kernels::Rung;
pub use error::Error;
