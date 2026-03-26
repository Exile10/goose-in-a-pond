//! MemoryRepository port — driven port for semantic memory persistence.

use crate::domain::memory::MemoryFragment;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: memory fragment persistence and similarity search.
///
/// When no `EmbeddingProvider` is wired, embeddings are `None` and
/// `search_similar` falls back to `search_recent` (recency-ordered results).
#[async_trait]
pub trait MemoryRepository: Send + Sync {
    /// Persist a new memory fragment.
    async fn add(&self, fragment: MemoryFragment) -> Result<()>;

    /// Return the `limit` most recent fragments for a profile, oldest-first.
    async fn search_recent(
        &self,
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>>;

    /// Return the top-`limit` fragments most similar to `query_embedding`.
    ///
    /// Similarity is computed via cosine similarity on stored BLOB vectors.
    /// Falls back to `search_recent` when no stored embeddings exist.
    async fn search_similar(
        &self,
        query_embedding: &[f32],
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>>;

    /// Delete a memory fragment by ID.
    async fn delete(&self, id: &str) -> Result<()>;
}
