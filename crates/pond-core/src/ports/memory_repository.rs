//! MemoryRepository port — driven port for semantic memory persistence.

use crate::domain::memory::{MemoryFragment, MemoryLifecycle, MemorySegment};
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

    /// Search active memories whose content matches any of the given keywords.
    ///
    /// Used for relevance-based retrieval: extract keywords from the user's
    /// message and find memories that mention those topics, regardless of age.
    /// Results are ordered by importance (highest first), then recency.
    async fn search_by_content(
        &self,
        keywords: &[String],
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        // Default: fall back to search_recent for adapters that don't implement this.
        let _ = keywords;
        self.search_recent(profile_id, limit).await
    }

    // ── Segment-aware methods (default no-op impls for backward compat) ──

    /// Increment access_count and update last_accessed_at for a memory.
    async fn record_access(&self, _id: &str) -> Result<()> {
        Ok(())
    }

    /// Update the lifecycle status of a memory.
    async fn update_lifecycle(&self, _id: &str, _lifecycle: MemoryLifecycle) -> Result<()> {
        Ok(())
    }

    /// Search active memories by segment.
    async fn search_by_segment(
        &self,
        _segment: MemorySegment,
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        self.search_recent(profile_id, limit).await
    }

    /// Retrieve all active memories that have decay fields for scoring.
    /// Used by the cleanup service to find archive/prune candidates.
    async fn search_scoreable(
        &self,
        profile_id: Option<&str>,
    ) -> Result<Vec<MemoryFragment>> {
        self.search_recent(profile_id, 1000).await
    }

    /// Batch update lifecycle for multiple memories at once.
    async fn batch_update_lifecycle(
        &self,
        _updates: &[(String, MemoryLifecycle)],
    ) -> Result<()> {
        Ok(())
    }

    /// Mark a memory as superseded by another (for consolidation).
    async fn mark_superseded(&self, _id: &str, _superseded_by: &str) -> Result<()> {
        Ok(())
    }
}
