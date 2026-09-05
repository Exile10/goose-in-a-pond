//! The single microphone owner for GIAP: one process, one input device, many subscribers.
//! [`ring`] is the shared rolling buffer and format normalisation, [`owner`] the thread actor
//! with its command protocol and privacy gate, [`cpal_device`] the real device, and [`testing`]
//! a scripted device so subscribers in other crates stay testable in CI.

pub mod cpal_device;
pub mod energy;
pub mod owner;
pub mod ring;
pub mod testing;

pub use cpal_device::{input_device_names, CpalCapture};
pub use energy::MicEnergy;
pub use owner::{spawn, CaptureDevice, MicCommand, MicHandle, MicReader, MicShared, MicState};
pub use ring::{Ring, CAPTURE_RATE_HZ};
