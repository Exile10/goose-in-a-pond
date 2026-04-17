//! FaceDetector port — optional face-localisation stage.
//!
//! A [`FaceDetector`] finds the primary face in an image and returns its
//! bounding box.  The embedding pipeline then crops to that box before
//! producing an embedding vector.
//!
//! # Why a separate port?
//!
//! Phase 2 ships with a center-square crop fallback in the ONNX embedder,
//! which works for well-framed headshots but fails on wide photos.  Wiring
//! mtCNN (or ULFG, RetinaFace, etc.) in front of the embedder is a Phase 2+
//! follow-up — this port is the seam that lets us drop a real detector in
//! without touching the rest of the pipeline.
//!
//! # No-op default
//!
//! [`NoopFaceDetector`] implements the trait by always returning `Ok(None)`,
//! which is semantically "I don't know where the face is — let the embedder
//! fall back to its default crop".  Use it as a placeholder when wiring
//! [`crate::ports::face_recognition`] in environments that do not bundle a
//! detection model.

use crate::domain::face_recognition::BoundingBox;
use anyhow::Result;
use async_trait::async_trait;

/// Driven port: locate the primary face in an image.
#[async_trait]
pub trait FaceDetector: Send + Sync {
    /// Detect the primary face in `image_bytes`.
    ///
    /// Returns `Ok(Some(bbox))` when a face is confidently located,
    /// `Ok(None)` when no face is detected or the detector is unsure,
    /// and `Err` only for infrastructure failures (model load, decode).
    async fn detect_face(&self, image_bytes: &[u8]) -> Result<Option<BoundingBox>>;
}

/// No-op detector: always returns `Ok(None)`.  Callers fall back to
/// center-square cropping.  Safe default when no detection model is bundled.
pub struct NoopFaceDetector;

#[async_trait]
impl FaceDetector for NoopFaceDetector {
    async fn detect_face(&self, _image_bytes: &[u8]) -> Result<Option<BoundingBox>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_detector_returns_none() {
        let d = NoopFaceDetector;
        assert!(d.detect_face(b"whatever").await.unwrap().is_none());
    }
}
