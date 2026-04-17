//! FaceRecognition port — public biometric identity differentiation.
//!
//! Exposes register / identify / delete operations over household-member face
//! embeddings.  Implementors compose a [`crate::ports::face_embedding_extractor::FaceEmbeddingExtractor`]
//! with a persistent store (typically SQLite) and an in-memory cosine-similarity
//! search to perform identification.
//!
//! # Privacy guarantees
//! - Raw image bytes MUST be discarded immediately after embedding extraction.
//! - Stored embeddings are opaque BLOBs and are not reconstructable back to
//!   the original biometric.
//! - `delete_embeddings` removes every row for a profile to satisfy the
//!   "forget all biometric data" requirement of the onboarding contract.

use crate::domain::face_recognition::{BoundingBox, FaceEmbedding, FaceIdentification};
use anyhow::Result;
use async_trait::async_trait;

/// Driven port: biometric face registration + matching.
#[async_trait]
pub trait FaceRecognition: Send + Sync {
    /// Enroll a new face for `profile_id`.  Decodes the image, extracts an
    /// embedding, and persists it.  Multiple enrollments per profile are
    /// allowed (improves identification accuracy).
    ///
    /// Returns the stored [`FaceEmbedding`] row.
    async fn register_face(
        &self,
        profile_id: &str,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
    ) -> Result<FaceEmbedding>;

    /// Identify the face in `image_bytes` against all enrolled embeddings.
    ///
    /// Returns a [`FaceIdentification`] indicating whether a match was found,
    /// the best candidate profile, and the cosine similarity score.
    ///
    /// `bbox` optionally narrows the image to a face region (client-supplied
    /// crop or detector output).  If `None`, implementors may fall back to a
    /// center-square crop.
    async fn identify_face(
        &self,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
    ) -> Result<FaceIdentification>;

    /// List all enrolled embeddings for a profile (useful for diagnostics
    /// and for the onboarding UI to show enrollment progress).
    async fn list_embeddings(&self, profile_id: &str) -> Result<Vec<FaceEmbedding>>;

    /// Delete every stored face embedding for a profile.  Used by the
    /// `DELETE /api/v1/users/:id/biometrics` endpoint.
    async fn delete_embeddings(&self, profile_id: &str) -> Result<u64>;

    /// Cosine-similarity threshold above which an identification is
    /// considered conclusive.  Defaults vary by model family (see issue).
    fn match_threshold(&self) -> f32;
}
