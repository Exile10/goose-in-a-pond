//! Face recognition domain types.
//!
//! `FaceEmbedding` is the stored representation of a household member's facial
//! identity. Raw images are never retained — only the compact float vector
//! produced by the embedding model is persisted.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A stored face embedding for a household member profile.
///
/// Embeddings are model-specific opaque vectors.  The `model_dims` field
/// records the dimensionality so callers can verify they are comparing
/// embeddings from the same model family (ArcFace-512 vs MobileFaceNet-128).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceEmbedding {
    /// Unique row ID (UUID v4).
    pub id: String,
    /// The household member profile this embedding belongs to.
    pub profile_id: String,
    /// Compact float representation of the face — never reconstructable
    /// back to the original image.
    pub embedding: Vec<f32>,
    /// Dimensionality of the embedding vector (e.g. 512 for ArcFace,
    /// 128 for MobileFaceNet).  Used as a sanity-check when comparing.
    pub model_dims: u32,
    /// When this embedding was enrolled.
    pub created_at: DateTime<Utc>,
}

/// Result of a face identification attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceIdentification {
    /// The identified profile, if confidence exceeded the threshold.
    pub profile_id: Option<String>,
    /// Cosine similarity score in \[0.0, 1.0\].
    /// `None` if the image contained no detectable face.
    pub confidence: Option<f32>,
    /// Whether the identification was conclusive (confidence ≥ threshold).
    pub identified: bool,
}

impl FaceIdentification {
    /// Build a positive identification result.
    pub fn found(profile_id: String, confidence: f32) -> Self {
        Self {
            profile_id: Some(profile_id),
            confidence: Some(confidence),
            identified: true,
        }
    }

    /// Build a result where no stored face matched.
    pub fn unknown(confidence: Option<f32>) -> Self {
        Self {
            profile_id: None,
            confidence,
            identified: false,
        }
    }

    /// Build a result where no face was detectable in the image.
    pub fn no_face() -> Self {
        Self {
            profile_id: None,
            confidence: None,
            identified: false,
        }
    }
}

/// Integer pixel bounding box around a face within a source image.
///
/// Coordinates are in the source image's pixel space (origin top-left).
/// Supplied by the client (UI crop), a face detector (mtCNN follow-up),
/// or synthesised by the adapter's center-square fallback when absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl BoundingBox {
    /// Parse `"x,y,w,h"` (four unsigned integers, comma-separated).
    pub fn parse_csv(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split(',').map(|p| p.trim()).collect();
        if parts.len() != 4 {
            return None;
        }
        Some(Self {
            x: parts[0].parse().ok()?,
            y: parts[1].parse().ok()?,
            width: parts[2].parse().ok()?,
            height: parts[3].parse().ok()?,
        })
    }
}
