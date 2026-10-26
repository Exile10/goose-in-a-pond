//! Opens a camera for the pipeline.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use pond_core::user_data::ports::vision::FrameSource;

use crate::ffmpeg_source::{CaptureConfig, FfmpegFrameSource};
use crate::live_feed::CapturePath;

/// A camera opened for the pipeline, and the path its frames take.
pub struct OpenedSource {
    pub source: Box<dyn FrameSource>,
    pub capture: CapturePath,
}

pub type OpenFuture = Pin<Box<dyn Future<Output = Result<OpenedSource>> + Send>>;

pub async fn open_capture(cfg: CaptureConfig) -> Result<OpenedSource> {
    Ok(OpenedSource {
        source: Box::new(FfmpegFrameSource::spawn(&cfg)?),
        capture: CapturePath::Software,
    })
}
