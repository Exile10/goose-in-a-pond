//! Vision event detection (#130, Q2-53).
//!
//! Camera capture + on-device detection feeding the same `camera_events`
//! store and EventBus that the external camera-event API uses, so #92
//! automation rules react to on-device detections with zero extra wiring:
//!
//! ```text
//! FfmpegFrameSource ─► MotionDetector ─► [VisionClassifier?] ─► CameraEvent
//!    (RTSP / V4L2)      (frame diff)      (pet/package model)      │
//!                                              CameraStorage ◄─────┤
//!                                              EventBus ◄──────────┘ → #92 rules
//! ```
//!
//! Everything runs on-device: capture via a local ffmpeg subprocess, motion
//! via pure-Rust frame differencing, optional labelling via a small local
//! model behind the [`VisionClassifier`] port.
//!
//! [`VisionClassifier`]: pond_core::user_data::ports::vision::VisionClassifier

mod ffmpeg_source;
mod motion;
mod pipeline;

pub use ffmpeg_source::{CaptureConfig, FfmpegFrameSource};
pub use motion::{MotionConfig, MotionDetector};
pub use pipeline::{run_vision_pipeline, VisionPipelineConfig};
