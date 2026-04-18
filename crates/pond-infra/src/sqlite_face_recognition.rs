//! SQLite-backed implementation of the [`FaceRecognition`] port.
//!
//! Composes a [`FaceEmbeddingExtractor`] (ONNX) with on-disk persistence and
//! in-memory cosine-similarity search.  Embeddings are packed as little-endian
//! f32 BLOBs in the `face_embeddings` table (see migration 0013).
//!
//! # Matching strategy
//! Identification loads every enrolled embedding into memory and computes the
//! maximum cosine similarity against the query.  For household-scale use
//! (typically < 50 enrollments) this is strictly faster than any ANN index
//! and avoids the complexity of a vector extension.  If the data set grows
//! materially larger, swap in an ANN (HNSW / sqlite-vec) without changing
//! the public port.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use pond_core::domain::face_recognition::{BoundingBox, FaceEmbedding, FaceIdentification};
use pond_core::ports::face_embedding_extractor::FaceEmbeddingExtractor;
use pond_core::ports::face_recognition::FaceRecognition;
use sqlx::{Pool, Sqlite};
use std::sync::Arc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Default cosine-similarity threshold for ArcFace-512 (per issue spec).
const DEFAULT_MATCH_THRESHOLD: f32 = 0.60;

pub struct SqliteFaceRecognition {
    pool: Pool<Sqlite>,
    extractor: Arc<dyn FaceEmbeddingExtractor>,
    model_name: String,
    threshold: f32,
}

impl SqliteFaceRecognition {
    /// Build a new face-recognition service backed by `pool` and `extractor`.
    /// Defaults: `model_name = "arcface"`, `threshold = 0.60`.
    pub fn new(pool: Pool<Sqlite>, extractor: Arc<dyn FaceEmbeddingExtractor>) -> Self {
        Self {
            pool,
            extractor,
            model_name: "arcface".to_string(),
            threshold: DEFAULT_MATCH_THRESHOLD,
        }
    }

    /// Override the model identifier recorded alongside each embedding.
    pub fn with_model_name(mut self, name: impl Into<String>) -> Self {
        self.model_name = name.into();
        self
    }

    /// Override the cosine-similarity threshold for positive identification.
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold;
        self
    }
}

#[derive(sqlx::FromRow)]
struct FaceRow {
    id:         String,
    profile_id: String,
    embedding:  Vec<u8>,
    model_dims: i64,
    created_at: String,
}

fn parse_dt(s: &str) -> chrono::DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or_else(|_| Utc::now())
}

/// Pack a `Vec<f32>` as little-endian bytes for BLOB storage.
fn pack_embedding(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Unpack a little-endian f32 BLOB back into a `Vec<f32>`.
fn unpack_embedding(bytes: &[u8]) -> Result<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return Err(anyhow!(
            "embedding BLOB length {} is not a multiple of 4",
            bytes.len()
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn row_to_embedding(row: FaceRow) -> Result<FaceEmbedding> {
    Ok(FaceEmbedding {
        id:         row.id,
        profile_id: row.profile_id,
        embedding:  unpack_embedding(&row.embedding)?,
        model_dims: row.model_dims as u32,
        created_at: parse_dt(&row.created_at),
    })
}

/// Cosine similarity in \[-1.0, 1.0\].  Returns 0.0 for zero-norm vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[async_trait]
impl FaceRecognition for SqliteFaceRecognition {
    async fn register_face(
        &self,
        profile_id: &str,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
    ) -> Result<FaceEmbedding> {
        let embedding = self
            .extractor
            .extract_embedding(image_bytes, bbox)
            .await
            .context("face embedding extraction failed")?
            .ok_or_else(|| anyhow!("no face detected in enrollment image"))?;

        let dims = self.extractor.embedding_dims();
        if embedding.len() != dims as usize {
            return Err(anyhow!(
                "extractor returned {} dims but advertises {}",
                embedding.len(),
                dims
            ));
        }

        let id = Uuid::new_v4().to_string();
        let packed = pack_embedding(&embedding);

        sqlx::query(
            "INSERT INTO face_embeddings \
             (id, profile_id, embedding, model_dims, model_name, created_at) \
             VALUES (?, ?, ?, ?, ?, datetime('now'))",
        )
        .bind(&id)
        .bind(profile_id)
        .bind(&packed)
        .bind(dims as i64)
        .bind(&self.model_name)
        .execute(&self.pool)
        .await
        .context("failed to insert face embedding")?;

        info!(%profile_id, %id, dims, "face embedding enrolled");

        Ok(FaceEmbedding {
            id,
            profile_id: profile_id.to_string(),
            embedding,
            model_dims: dims,
            created_at: Utc::now(),
        })
    }

    async fn identify_face(
        &self,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
    ) -> Result<FaceIdentification> {
        let query_embedding = match self
            .extractor
            .extract_embedding(image_bytes, bbox)
            .await
            .context("face embedding extraction failed")?
        {
            Some(e) => e,
            None => {
                debug!("identify_face: no face detected");
                return Ok(FaceIdentification::no_face());
            }
        };

        let dims = self.extractor.embedding_dims() as i64;

        // Only compare against embeddings from the same model family.
        let rows: Vec<FaceRow> = sqlx::query_as(
            "SELECT id, profile_id, embedding, model_dims, created_at \
             FROM face_embeddings WHERE model_dims = ?",
        )
        .bind(dims)
        .fetch_all(&self.pool)
        .await
        .context("failed to load face embeddings for matching")?;

        if rows.is_empty() {
            return Ok(FaceIdentification::unknown(None));
        }

        let mut best_score = -1.0_f32;
        let mut best_profile: Option<String> = None;

        for row in rows {
            let candidate = match unpack_embedding(&row.embedding) {
                Ok(v) => v,
                Err(e) => {
                    warn!(id = %row.id, "skipping malformed embedding: {e}");
                    continue;
                }
            };
            let score = cosine_similarity(&query_embedding, &candidate);
            if score > best_score {
                best_score = score;
                best_profile = Some(row.profile_id);
            }
        }

        let confidence = best_score.max(0.0);
        match best_profile {
            Some(profile_id) if best_score >= self.threshold => {
                info!(%profile_id, confidence, "face identified");
                Ok(FaceIdentification::found(profile_id, confidence))
            }
            _ => {
                debug!(confidence, "face not identified (below threshold)");
                Ok(FaceIdentification::unknown(Some(confidence)))
            }
        }
    }

    async fn list_embeddings(&self, profile_id: &str) -> Result<Vec<FaceEmbedding>> {
        let rows: Vec<FaceRow> = sqlx::query_as(
            "SELECT id, profile_id, embedding, model_dims, created_at \
             FROM face_embeddings WHERE profile_id = ? \
             ORDER BY created_at DESC, id DESC",
        )
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list face embeddings")?;

        rows.into_iter().map(row_to_embedding).collect()
    }

    async fn delete_embeddings(&self, profile_id: &str) -> Result<u64> {
        let result = sqlx::query("DELETE FROM face_embeddings WHERE profile_id = ?")
            .bind(profile_id)
            .execute(&self.pool)
            .await
            .context("failed to delete face embeddings")?;
        let n = result.rows_affected();
        info!(%profile_id, deleted = n, "face embeddings deleted");
        Ok(n)
    }

    fn match_threshold(&self) -> f32 {
        self.threshold
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::sqlite_profile::SqliteProfileRepository;
    use async_trait::async_trait;
    use pond_core::domain::profile::CreateProfileRequest;
    use pond_core::ports::profile::ProfileRepository;
    use tempfile::tempdir;

    /// Deterministic stub extractor: returns a fixed embedding keyed by the
    /// first byte of the input.  Lets us exercise storage + matching without
    /// loading any real ONNX models.
    struct StubExtractor {
        dims: u32,
    }

    #[async_trait]
    impl FaceEmbeddingExtractor for StubExtractor {
        async fn extract_embedding(
            &self,
            image_bytes: &[u8],
            _bbox: Option<BoundingBox>,
        ) -> Result<Option<Vec<f32>>> {
            if image_bytes.is_empty() {
                return Ok(None);
            }
            // Build a unit vector that points in a "direction" determined
            // by the first byte — two images sharing the same first byte
            // will match with cosine similarity 1.0.
            let mut v = vec![0.0_f32; self.dims as usize];
            let slot = (image_bytes[0] as usize) % (self.dims as usize);
            v[slot] = 1.0;
            Ok(Some(v))
        }
        fn embedding_dims(&self) -> u32 {
            self.dims
        }
    }

    async fn setup() -> (SqliteFaceRecognition, String, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let profiles = SqliteProfileRepository::new(db.system.clone());
        let profile = profiles
            .create(CreateProfileRequest {
                display_name: "Alice".into(),
                avatar_emoji: "\u{1F986}".into(),
            })
            .await
            .unwrap();
        let svc = SqliteFaceRecognition::new(
            db.system,
            Arc::new(StubExtractor { dims: 128 }),
        );
        (svc, profile.id, tmp)
    }

    #[tokio::test]
    async fn register_and_list_round_trip() {
        let (svc, profile_id, _tmp) = setup().await;
        let stored = svc.register_face(&profile_id, &[7u8, 1, 2], None).await.unwrap();
        assert_eq!(stored.profile_id, profile_id);
        assert_eq!(stored.embedding.len(), 128);

        let listed = svc.list_embeddings(&profile_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, stored.id);
    }

    #[tokio::test]
    async fn identify_returns_enrolled_profile() {
        let (svc, profile_id, _tmp) = setup().await;
        svc.register_face(&profile_id, &[42u8], None).await.unwrap();

        let result = svc.identify_face(&[42u8, 99, 100], None).await.unwrap();
        assert!(result.identified);
        assert_eq!(result.profile_id.as_deref(), Some(profile_id.as_str()));
        assert!(result.confidence.unwrap() >= 0.6);
    }

    #[tokio::test]
    async fn identify_returns_unknown_below_threshold() {
        let (svc, profile_id, _tmp) = setup().await;
        svc.register_face(&profile_id, &[1u8], None).await.unwrap();

        // First byte differs → orthogonal embedding → similarity ≈ 0
        let result = svc.identify_face(&[200u8], None).await.unwrap();
        assert!(!result.identified);
        assert!(result.profile_id.is_none());
    }

    #[tokio::test]
    async fn identify_without_enrollments_returns_unknown() {
        let (svc, _profile_id, _tmp) = setup().await;
        let result = svc.identify_face(&[1u8], None).await.unwrap();
        assert!(!result.identified);
        assert!(result.profile_id.is_none());
    }

    #[tokio::test]
    async fn identify_returns_no_face_on_empty_image() {
        let (svc, profile_id, _tmp) = setup().await;
        svc.register_face(&profile_id, &[1u8], None).await.unwrap();
        let result = svc.identify_face(&[], None).await.unwrap();
        assert!(!result.identified);
        assert!(result.confidence.is_none());
    }

    #[tokio::test]
    async fn delete_removes_all_embeddings_for_profile() {
        let (svc, profile_id, _tmp) = setup().await;
        svc.register_face(&profile_id, &[1u8], None).await.unwrap();
        svc.register_face(&profile_id, &[2u8], None).await.unwrap();
        svc.register_face(&profile_id, &[3u8], None).await.unwrap();

        let n = svc.delete_embeddings(&profile_id).await.unwrap();
        assert_eq!(n, 3);
        assert!(svc.list_embeddings(&profile_id).await.unwrap().is_empty());
    }

    #[test]
    fn pack_unpack_is_lossless() {
        let v = vec![0.0_f32, 1.5, -3.25, std::f32::consts::PI];
        let bytes = pack_embedding(&v);
        let back = unpack_embedding(&bytes).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn cosine_similarity_basic_cases() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&a, &c).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[], &[0.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }
}
