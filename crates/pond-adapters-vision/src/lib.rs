//! Vision event detection (#130, Q2-53). Capture through a local ffmpeg
//! subprocess, pure-Rust motion differencing, optional labelling by a local
//! model, all feeding the same `camera_events` store and EventBus as the external
//! camera-event API, so #92 automation rules react with zero extra wiring.

mod ffmpeg_source;
mod motion;
mod pipeline;
mod snapshot;

pub use ffmpeg_source::{CaptureConfig, FfmpegFrameSource};
pub use motion::{MotionConfig, MotionDetector};
pub use pipeline::{run_vision_pipeline, VisionPipelineConfig};
pub use snapshot::SnapshotConfig;
