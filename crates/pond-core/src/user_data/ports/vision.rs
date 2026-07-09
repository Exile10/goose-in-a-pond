//! Driven ports for the vision pipeline (#130).
//!
//! [`FrameSource`] abstracts *where frames come from* (an RTSP camera, a V4L2
//! device, a scripted test source); [`VisionClassifier`] abstracts *what is in
//! a frame* (pet/package/person via a small on-device model). The pipeline in
//! `pond-adapters-vision` consumes both and emits `CameraEvent`s.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::vision::{Detection, Frame};

/// Driven port: a stream of captured camera frames.
///
/// `Send` (not `Sync`): a source is owned and polled by exactly one pipeline
/// task.
#[async_trait]
pub trait FrameSource: Send {
    /// The next frame, or `Ok(None)` when the stream has ended.
    async fn next_frame(&mut self) -> Result<Option<Frame>>;
}

/// Driven port: classify what a frame shows (pet / package / person …).
///
/// Implementations run **on-device** (e.g. a small ONNX model via the system
/// ONNX Runtime). The pipeline treats the classifier as optional enrichment:
/// with no classifier configured, motion events are emitted unlabelled.
#[async_trait]
pub trait VisionClassifier: Send + Sync {
    /// Detections for `frame`, best first. An empty vec means
    /// "nothing recognized" (the caller falls back to a plain motion event).
    async fn classify(&self, frame: &Frame) -> Result<Vec<Detection>>;
}
