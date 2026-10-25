//! Vision events (ffmpeg capture, motion differencing, optional local labelling) feed the same
//! `camera_events` store and EventBus as the external camera-event API.

mod discovery;
mod ffmpeg_source;
mod live_feed;
mod motion;
mod pipeline;
mod snapshot;

pub use discovery::{default_camera_source, detect_v4l2_cameras, DiscoveredCamera};
pub use ffmpeg_source::{CaptureConfig, FfmpegFrameSource};
pub use live_feed::{LiveCameraRegistry, VisionBusConfig};
pub use motion::{MotionConfig, MotionDetector};
pub use pipeline::{run_vision_pipeline, VisionPipelineConfig};
pub use snapshot::{frame_to_jpeg_bytes, write_snapshot, SnapshotConfig};
