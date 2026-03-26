//! EmbeddingProvider port — driven port for generating text embeddings.
//!
//! Wire a real model (e.g. fastembed-rs with MiniLM-L6-v2) when available.
//! Use `MockEmbeddingProvider` in tests or when no model is configured.

use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: text embedding generation.
///
/// Takes raw text and returns a fixed-length float vector.
/// All vectors returned by the same provider must have the same dimensionality.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Generate an embedding vector for the given text.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// The dimension of vectors returned by this provider (e.g. 384 for MiniLM).
    fn dimensions(&self) -> usize;
}
