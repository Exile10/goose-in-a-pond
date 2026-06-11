//! Fastembed adapter for the `EmbeddingProvider` port.
//!
//! Wraps [`fastembed::TextEmbedding`] to generate dense vectors from text using
//! ONNX-based models (default: `all-MiniLM-L6-v2`, 384 dimensions).
//!
//! The model is auto-downloaded from HuggingFace on first use and cached under
//! the provided `cache_dir` (typically `$DATA_DIR/models/embedding/`).
//!
//! # Thread Safety
//!
//! `TextEmbedding` is not `Send`/`Sync`, so inference is dispatched to a
//! blocking thread via `tokio::task::spawn_blocking`. The inner model is
//! wrapped in a `Mutex` to serialize access — acceptable for GIAP's workload
//! (1-3 embeddings per chat turn during memory extraction).

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use pond_core::models::ports::embedding::EmbeddingProvider;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Known embedding models and their dimension counts.
const MINILM_L6_V2_DIMS: usize = 384;
const BGE_SMALL_EN_DIMS: usize = 384;

/// Resolves a model name string to a fastembed `EmbeddingModel` enum variant
/// and its output dimensionality.
fn resolve_model(name: &str) -> Result<(EmbeddingModel, usize)> {
    match name {
        "all-MiniLM-L6-v2" | "" => Ok((EmbeddingModel::AllMiniLML6V2, MINILM_L6_V2_DIMS)),
        "bge-small-en-v1.5" => Ok((EmbeddingModel::BGESmallENV15, BGE_SMALL_EN_DIMS)),
        other => Err(anyhow!(
            "Unknown embedding model: '{}'. Supported: all-MiniLM-L6-v2, bge-small-en-v1.5",
            other
        )),
    }
}

/// `EmbeddingProvider` implementation backed by fastembed (ONNX Runtime).
///
/// Construction is blocking (loads ONNX model into memory). Callers should
/// construct on a blocking thread or during startup before the async runtime
/// needs the provider.
pub struct FastembedEmbeddingProvider {
    model: Arc<Mutex<TextEmbedding>>,
    dims: usize,
    model_name: String,
}

impl FastembedEmbeddingProvider {
    /// Create a new provider for `model_name`.
    ///
    /// `cache_dir` is where fastembed stores downloaded ONNX artefacts.
    /// Pass `None` to use fastembed's default cache location.
    ///
    /// Supported models:
    /// - `"all-MiniLM-L6-v2"` (default, 384-dim, ~23 MB)
    /// - `"bge-small-en-v1.5"` (384-dim, ~33 MB)
    pub fn new(model_name: &str, cache_dir: Option<PathBuf>) -> Result<Self> {
        let (variant, dims) = resolve_model(model_name)?;

        let mut opts = InitOptions::new(variant).with_show_download_progress(true);

        if let Some(dir) = cache_dir {
            // Ensure the cache directory exists before handing it to fastembed.
            std::fs::create_dir_all(&dir)?;
            opts = opts.with_cache_dir(dir);
        }

        let embedding = TextEmbedding::try_new(opts)?;

        let effective_name = if model_name.is_empty() {
            "all-MiniLM-L6-v2"
        } else {
            model_name
        };

        tracing::info!(
            model = effective_name,
            dims = dims,
            "fastembed embedding model loaded"
        );

        Ok(Self {
            model: Arc::new(Mutex::new(embedding)),
            dims,
            model_name: effective_name.to_string(),
        })
    }

    /// Human-readable model name (for logging / status endpoints).
    pub fn model_name(&self) -> &str {
        &self.model_name
    }
}

#[async_trait]
impl EmbeddingProvider for FastembedEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let model = Arc::clone(&self.model);
        let owned = text.to_string();

        // Dispatch to a blocking thread — ONNX inference is CPU-bound and must
        // not block the tokio runtime.
        let result = tokio::task::spawn_blocking(move || {
            let mut guard = model
                .lock()
                .map_err(|e| anyhow!("fastembed mutex poisoned: {e}"))?;
            let embeddings = guard
                .embed(vec![owned], None)
                .map_err(|e| anyhow!("fastembed embed failed: {e}"))?;
            embeddings
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("fastembed returned empty embeddings"))
        })
        .await
        .map_err(|e| anyhow!("spawn_blocking join error: {e}"))??;

        Ok(result)
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_known_models() {
        let (model, dims) = resolve_model("all-MiniLM-L6-v2").unwrap();
        assert_eq!(dims, 384);
        assert!(matches!(model, EmbeddingModel::AllMiniLML6V2));

        let (model, dims) = resolve_model("bge-small-en-v1.5").unwrap();
        assert_eq!(dims, 384);
        assert!(matches!(model, EmbeddingModel::BGESmallENV15));
    }

    #[test]
    fn resolve_empty_defaults_to_minilm() {
        let (model, dims) = resolve_model("").unwrap();
        assert_eq!(dims, 384);
        assert!(matches!(model, EmbeddingModel::AllMiniLML6V2));
    }

    #[test]
    fn resolve_unknown_model_errors() {
        let err = resolve_model("nonexistent-model").unwrap_err();
        assert!(err.to_string().contains("Unknown embedding model"));
    }
}
