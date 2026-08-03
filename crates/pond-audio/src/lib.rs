//! The single microphone owner for GIAP.
//!
//! One process, one input device, many subscribers. Replaces three subsystems
//! that each opened `default_input_device()` independently — the wake-word
//! detector, the capture path, and piper's barge-in listener — which `run_loop`
//! could have live simultaneously.
//!
//! - [`ring`] — the shared rolling buffer and format normalisation (pure).
//! - [`owner`] — the thread actor, its command protocol and privacy gate.
//! - [`cpal_device`] — the real device. Deliberately thin.
//! - [`testing`] — a scripted device so SUBSCRIBERS in other crates become
//!   testable in CI; none of them had tests when each opened its own device.

pub mod cpal_device;
pub mod energy;
pub mod owner;
pub mod ring;
pub mod testing;

pub use cpal_device::{input_device_names, CpalCapture};
pub use energy::MicEnergy;
pub use owner::{spawn, CaptureDevice, MicCommand, MicHandle, MicReader, MicShared, MicState};
pub use ring::{Ring, CAPTURE_RATE_HZ};
