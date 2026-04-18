//! FaceEmbeddingExtractor port — driven port for computing face embeddings.
//!
//! Implementors run a two-stage ONNX pipeline:
//!   1. **Detection** (mtCNN) — locates and crops the largest face in the image.
//!   2. **Embedding** (ArcFace-512 or MobileFaceNet-128) — encodes the crop as
//!      a compact float vector suitable for cosine-similarity comparison.
//!
//! This port is intentionally narrow: it only extracts the embedding.
//! Persistence and matching are handled by [`crate::ports::face_recognition`].
//!
//! # Privacy
//! Implementations MUST NOT write raw image pixels or intermediate crops to
//! disk.  Only the embedding vector may leave the method boundary.

use crate::domain::face_recognition::BoundingBox;
use anyhow::Result;
use async_trait::async_trait;

/// Driven port: compute a face embedding from raw image bytes.
///
/// Implementors are expected to:
/// - Accept any common image format (JPEG, PNG, WebP) in `image_bytes`.
/// - Detect the primary face in the image.
/// - Return a normalised float embedding vector.
/// - Return `Ok(None)` when no face is detectable.
/// - Discard all intermediate pixel data before returning.
#[async_trait]
pub trait FaceEmbeddingExtractor: Send + Sync {
    /// Extract a face embedding from `image_bytes`.
    ///
    /// Returns `Ok(None)` when no face is detected.
    /// Returns `Err` only on infrastructure failures (model not loaded,
    /// image decode error, etc.).
    async fn extract_embedding(
        &self,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
    ) -> Result<Option<Vec<f32>>>;

    /// Dimensionality of the embeddings this extractor produces.
    /// Used to validate compatibility when comparing stored embeddings.
    fn embedding_dims(&self) -> u32;
}
