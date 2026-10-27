//! Driven ports consumed by the vision pipeline in `pond-adapters-vision`.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::vision::{CapturedFrame, Detection, Frame};

/// A stream of captured camera frames; `Send` only, as one pipeline task owns and polls it.
#[async_trait]
pub trait FrameSource: Send {
    /// The next frame, or `Ok(None)` when the stream has ended.
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>>;

    /// Release the camera and wait until it is free; a device reopened before this returns can
    /// still be held by the old capture.
    async fn close(&mut self) {}
}

/// Classifies what a frame shows (pet, package, person); implementations run on-device.
#[async_trait]
pub trait VisionClassifier: Send + Sync {
    /// Detections for `frame`, best first; empty means nothing was recognized.
    async fn classify(&self, frame: &Frame) -> Result<Vec<Detection>>;
}
