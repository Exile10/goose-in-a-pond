//! GGUF embedding adapter — implements `EmbeddingProvider` via llama-cpp-2.
//!
//! Loads a GGUF embedding model (e.g. all-MiniLM-L6-v2, nomic-embed-text)
//! directly into the process using the shared `InferenceRuntime` backend.
//! Each `embed()` call creates a lightweight context, decodes the tokens,
//! and reads the pooled embedding vector.
//!
//! # Memory budget
//!
//! Embedding models are tiny (20-100 MB). The model stays loaded for the
//! lifetime of the adapter. On Jetson Orin Nano this is negligible next to
//! the 2-3 GB chat model.
//!
//! # Thread safety
//!
//! `LlamaModel` is `Send + Sync`. Each `embed()` call creates its own
//! `LlamaContext` — no shared mutable state, so concurrent calls are safe.

use anyhow::{Context, Result};
use async_trait::async_trait;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use pond_core::models::ports::embedding::EmbeddingProvider;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;

use goose::providers::local_inference::InferenceRuntime;

/// Maximum context length for embedding models.
///
/// Embedding models typically support 256-512 tokens. 512 is generous
/// and keeps memory usage minimal (~2 MB KV cache for a 384-dim model).
const EMBEDDING_CTX_SIZE: u32 = 512;

/// In-process GGUF embedding provider backed by llama-cpp-2.
///
/// Construct with [`GgufEmbeddingProvider::new`], passing the path to a
/// GGUF embedding model file. The model is loaded once at construction
/// and held for the adapter's lifetime.
pub struct GgufEmbeddingProvider {
    /// The shared inference runtime (owns the LlamaBackend).
    runtime: Arc<InferenceRuntime>,
    /// The loaded embedding model.
    model: LlamaModel,
    /// Cached embedding dimensionality (e.g. 384 for MiniLM-L6-v2).
    dims: usize,
}

impl GgufEmbeddingProvider {
    /// Load a GGUF embedding model from `model_path`.
    ///
    /// The model is loaded with full GPU offload (`n_gpu_layers = 99`)
    /// for both Metal (macOS) and CUDA (Jetson) builds. On CPU-only
    /// builds the flag is harmless — llama.cpp ignores it.
    ///
    /// # Errors
    ///
    /// Returns an error if the model file does not exist, is not a valid
    /// GGUF file, or if the llama backend fails to initialise.
    pub fn new(model_path: &Path) -> Result<Self> {
        anyhow::ensure!(
            model_path.exists(),
            "Embedding model not found: {}",
            model_path.display()
        );

        let runtime = InferenceRuntime::get_or_init();
        let backend = runtime.backend();

        let params = LlamaModelParams::default()
            .with_n_gpu_layers(99)
            .with_use_mlock(false);

        tracing::info!(
            path = %model_path.display(),
            "Loading GGUF embedding model"
        );

        let model =
            LlamaModel::load_from_file(backend, model_path, &params).with_context(|| {
                format!(
                    "Failed to load GGUF embedding model: {}",
                    model_path.display()
                )
            })?;

        let dims = model.n_embd() as usize;
        anyhow::ensure!(dims > 0, "Embedding model reports n_embd = 0");

        tracing::info!(
            dims,
            path = %model_path.display(),
            "GGUF embedding model loaded"
        );

        Ok(Self {
            runtime,
            model,
            dims,
        })
    }

    /// Tokenize, decode, and extract the pooled embedding for `text`.
    ///
    /// This is the synchronous inner implementation. The async `embed()`
    /// method wraps this in `spawn_blocking` to avoid blocking the tokio
    /// runtime (llama-cpp-2 decode is CPU/GPU-bound).
    fn embed_sync(&self, text: &str) -> Result<Vec<f32>> {
        // 1. Tokenize
        let tokens = self
            .model
            .str_to_token(text, AddBos::Always)
            .with_context(|| "Failed to tokenize text for embedding")?;

        anyhow::ensure!(!tokens.is_empty(), "Tokenization produced zero tokens");

        // Truncate to context size if needed (embedding models have short contexts)
        let max_tokens = EMBEDDING_CTX_SIZE as usize;
        let tokens = if tokens.len() > max_tokens {
            tracing::debug!(
                original_len = tokens.len(),
                truncated_to = max_tokens,
                "Truncating embedding input to context size"
            );
            &tokens[..max_tokens]
        } else {
            &tokens[..]
        };

        // 2. Create a temporary context with embeddings enabled
        let ctx_size = NonZeroU32::new(EMBEDDING_CTX_SIZE);
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(ctx_size)
            .with_embeddings(true);

        let mut ctx = self
            .model
            .new_context(self.runtime.backend(), ctx_params)
            .with_context(|| "Failed to create embedding context")?;

        // 3. Decode tokens
        let mut batch =
            LlamaBatch::get_one(tokens).with_context(|| "Failed to create embedding batch")?;

        ctx.decode(&mut batch)
            .with_context(|| "Failed to decode embedding batch")?;

        // 4. Read pooled embedding (sequence 0)
        let raw_embedding = ctx.embeddings_seq_ith(0).with_context(|| {
            "Failed to read pooled embedding (is this an embedding model with pooling enabled?)"
        })?;

        // 5. L2 normalize
        let embedding = l2_normalize(raw_embedding);

        Ok(embedding)
    }
}

#[async_trait]
impl EmbeddingProvider for GgufEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // embed_sync involves CPU/GPU-bound llama-cpp-2 calls.
        // We cannot use spawn_blocking because LlamaModel is not Send
        // across the spawn_blocking boundary (it holds a raw pointer).
        // However, the actual decode is fast for embedding models (~1-5ms
        // for 384-dim), so running on the async thread is acceptable.
        // If this becomes a bottleneck, wrap in a dedicated thread with
        // a channel-based interface.
        self.embed_sync(text)
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

/// L2-normalize a vector in place, returning the normalized copy.
///
/// If the vector has zero magnitude (all zeros), returns it unchanged
/// to avoid division by zero.
fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_vector() {
        let v = vec![1.0, 0.0, 0.0];
        let n = l2_normalize(&v);
        assert!((n[0] - 1.0).abs() < 1e-6);
        assert!((n[1]).abs() < 1e-6);
        assert!((n[2]).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_general_vector() {
        let v = vec![3.0, 4.0];
        let n = l2_normalize(&v);
        // magnitude should be 1.0
        let mag: f32 = n.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((mag - 1.0).abs() < 1e-6);
        // 3/5 = 0.6, 4/5 = 0.8
        assert!((n[0] - 0.6).abs() < 1e-6);
        assert!((n[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_no_panic() {
        let v = vec![0.0, 0.0, 0.0];
        let n = l2_normalize(&v);
        assert_eq!(n, v);
    }

    #[test]
    fn embedding_ctx_size_is_reasonable() {
        assert!(EMBEDDING_CTX_SIZE >= 128);
        assert!(EMBEDDING_CTX_SIZE <= 2048);
    }
}
