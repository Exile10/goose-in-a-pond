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
    /// Generate an embedding vector for a stored DOCUMENT — a memory, a context
    /// item, a summary. Anything being written to the corpus.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Generate an embedding vector for a QUERY — the thing being searched WITH. Retrieval models
    /// are often asymmetric (nomic-embed-text wants `search_query: ` here and `search_document: `
    /// on stored text), so the wrong prefix costs ranking quality. Must return a vector in the
    /// SAME space and width as [`Self::embed`]: it selects a task prefix, never another model.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text).await
    }

    /// The dimension of vectors returned by this provider (e.g. 384 for MiniLM).
    fn dimensions(&self) -> usize;

    /// Stable identity of the model producing these vectors, stamped onto every personal-context
    /// row: a vector from a different embedder still scores plausibly and is wrong, so a mismatch
    /// must be detectable. Retrieval filters on it and the staleness sweep re-embeds on it. The
    /// default placeholder is self-consistent; override it wherever vectors outlive the process.
    fn model_id(&self) -> String {
        "unspecified".to_string()
    }
}
