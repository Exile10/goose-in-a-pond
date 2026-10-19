//! Finds the machine's microphones and speakers, tests them briefly, and keeps the pond on ones
//! that work. The policy is pond-core's; this crate is the cpal side and the watching.

pub mod names;
pub mod probe;
pub mod prober;
pub mod sound;
pub mod sound_server;
pub mod supervisor;
pub mod tone;

pub use prober::CpalProber;
pub use supervisor::{
    AudioPrefs, AudioSupervisor, ChangeFn, PondAudioControl, PrefsFn, Role, SupervisorConfig,
    Trigger,
};
