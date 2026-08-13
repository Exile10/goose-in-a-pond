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

    /// Generate an embedding vector for a QUERY — the thing being searched WITH,
    /// not the thing being searched.
    ///
    /// Retrieval models are often trained asymmetrically: nomic-embed-text, for
    /// instance, wants `search_document: ` on stored text and `search_query: ` on
    /// the question, and using the document form for both puts the query in the
    /// wrong manifold and costs ranking quality. This is a *provided* method, so
    /// a provider with no such distinction (fastembed, mocks) inherits the
    /// correct behaviour by doing nothing.
    ///
    /// It must return a vector in the SAME space and of the same width as
    /// [`Self::embed`] — it selects a task prefix, never a different model.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text).await
    }

    /// The dimension of vectors returned by this provider (e.g. 384 for MiniLM).
    fn dimensions(&self) -> usize;

    /// Stable identity of the model producing these vectors.
    ///
    /// Stamped onto every row in the personal-context index, because a vector
    /// from a different embedder still scores plausibly and is **wrong** — so a
    /// mismatch has to be detectable rather than silent. Retrieval filters on
    /// it; the staleness sweep re-embeds on it.
    ///
    /// The default is a placeholder on purpose: it is self-consistent (a
    /// provider that does not override it indexes and retrieves under the same
    /// name, so nothing mis-scores), but any provider whose vectors outlive the
    /// process should give its real model name so a model change is visible.
    fn model_id(&self) -> String {
        "unspecified".to_string()
    }
}
