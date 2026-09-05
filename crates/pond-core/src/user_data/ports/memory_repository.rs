//! MemoryRepository port — driven port for semantic memory persistence.

use crate::user_data::domain::memory::{
    MemoryEdge, MemoryEvent, MemoryEventKind, MemoryFragment, MemoryGraph, MemoryLifecycle,
    MemorySegment,
};
use crate::user_data::domain::profile::ProfileScope;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: memory fragment persistence and similarity search.
///
/// When no `EmbeddingProvider` is wired, embeddings are `None` and
/// `search_similar` falls back to `search_recent` (recency-ordered results).
///
/// # Scoping
///
/// Every read takes a [`ProfileScope`] rather than an `Option<&str>` profile id.
/// The `Option` was the reason the household shared one memory pool: `None` was
/// always reachable, so every production call site passed it. An enum makes each
/// caller choose, and makes `Guest` — see nothing — expressible at all.
#[async_trait]
pub trait MemoryRepository: Send + Sync {
    /// Persist a new memory fragment.
    async fn add(&self, fragment: MemoryFragment) -> Result<()>;

    /// Return the `limit` most recent fragments for a profile, oldest-first.
    async fn search_recent(
        &self,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>>;

    /// Return the top-`limit` fragments most similar to `query_embedding`.
    ///
    /// Similarity is computed via cosine similarity on stored BLOB vectors.
    /// Falls back to `search_recent` when no stored embeddings exist.
    async fn search_similar(
        &self,
        query_embedding: &[f32],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>>;

    /// Delete a memory fragment by ID.
    async fn delete(&self, id: &str) -> Result<()>;

    /// How many fragments belong to this member.
    ///
    /// Counts `profile_id = ?` **exactly** -- never the `IS NULL` rows that a
    /// [`ProfileScope::Owner`] read also returns. Those are shared household
    /// context, they survive the member, and reporting them as "deleted" would
    /// be a lie told at the one moment the user most needs the number to be
    /// true.
    ///
    /// [`ProfileScope::Owner`]: crate::user_data::domain::profile::ProfileScope::Owner
    async fn count_for_profile(&self, _profile_id: &str) -> Result<u64> {
        Ok(0) // default no-op for backward compat
    }

    /// Return up to `limit` active fragments that have no stored embedding.
    ///
    /// Drives the startup embedding backfill
    /// (`services::memory_relevance::run_backfill`): extraction historically
    /// stored `embedding: None`, which makes those rows invisible to
    /// `search_similar`. Adapters that cannot report this return nothing, and
    /// the backfill simply finds no work.
    async fn search_unembedded(&self, _limit: usize) -> Result<Vec<MemoryFragment>> {
        Ok(vec![])
    }

    /// Return up to `limit` active fragments whose stored vector is NOT
    /// `expected_dims` wide — i.e. produced by a different embedding model.
    ///
    /// [`Self::search_unembedded`] cannot find these: a stale vector is not
    /// NULL, so a pond that switched `embedding_provider` (fastembed's 384 to
    /// GGUF's 768) would keep rows that are excluded from semantic search and
    /// never repaired, degrading retrieval permanently and silently. This is
    /// what makes that recoverable.
    ///
    /// Width is the discriminator because nothing persists a model id yet; see
    /// `pond_inference::EmbeddingModelSpec`, where every GGUF model is 768
    /// precisely so a 384 vector is unambiguously a fastembed leftover.
    /// Adapters that cannot report this return nothing and the sweep finds no
    /// work — the same degrade-to-nothing contract as `search_unembedded`.
    async fn search_stale_dimension(
        &self,
        _expected_dims: usize,
        _limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        Ok(vec![])
    }

    /// Attach (or replace) the embedding vector of an existing fragment.
    async fn update_embedding(&self, _id: &str, _embedding: &[f32]) -> Result<()> {
        Ok(())
    }

    /// Search active memories whose content matches any of the given keywords.
    ///
    /// Used for relevance-based retrieval: extract keywords from the user's
    /// message and find memories that mention those topics, regardless of age.
    /// Results are ordered by importance (highest first), then recency.
    async fn search_by_content(
        &self,
        keywords: &[String],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        // Default: fall back to search_recent for adapters that don't implement this.
        let _ = keywords;
        self.search_recent(scope, limit).await
    }

    // ── Segment-aware methods (default no-op impls for backward compat) ──

    /// Increment access_count and update last_accessed_at for a memory.
    async fn record_access(&self, _id: &str) -> Result<()> {
        Ok(())
    }

    /// Update the lifecycle status of a memory.
    /// Replace a memory's text in place, keeping its identity.
    ///
    /// The desktop used to edit by ADDING the new text and DELETING the old
    /// row, which changes the id, resets `created_at` and `access_count`, and
    /// orphans the vector and any edge pointing at it — so a corrected memory
    /// came back as a brand-new one that had never been used. It also leaves a
    /// duplicate behind if the delete half fails.
    ///
    /// The embedding is CLEARED rather than kept: it describes the old words,
    /// and a vector that no longer matches its text is worse than no vector,
    /// because it still scores. The maintenance sweep re-embeds it.
    async fn update_content(&self, _id: &str, _content: &str) -> Result<()> {
        anyhow::bail!("this store cannot edit a memory's text")
    }

    async fn update_lifecycle(&self, _id: &str, _lifecycle: MemoryLifecycle) -> Result<()> {
        Ok(())
    }

    /// Search active memories by segment.
    async fn search_by_segment(
        &self,
        _segment: MemorySegment,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        self.search_recent(scope, limit).await
    }

    /// Retrieve all active memories that have decay fields for scoring.
    /// Used by the cleanup service to find archive/prune candidates.
    async fn search_scoreable(&self, scope: &ProfileScope) -> Result<Vec<MemoryFragment>> {
        self.search_recent(scope, 1000).await
    }

    /// Batch update lifecycle for multiple memories at once.
    async fn batch_update_lifecycle(&self, _updates: &[(String, MemoryLifecycle)]) -> Result<()> {
        Ok(())
    }

    /// Mark a memory as superseded by another (for consolidation).
    async fn mark_superseded(&self, _id: &str, _superseded_by: &str) -> Result<()> {
        Ok(())
    }

    // ── Graph edge methods (default no-op impls for backward compat) ────

    /// Persist a directed edge between two memories.
    async fn add_edge(&self, _edge: MemoryEdge) -> Result<()> {
        Ok(())
    }

    /// Return all edges originating from `memory_id`.
    async fn get_edges_from(&self, _memory_id: &str) -> Result<Vec<MemoryEdge>> {
        Ok(vec![])
    }

    /// Return all edges pointing to `memory_id`.
    async fn get_edges_to(&self, _memory_id: &str) -> Result<Vec<MemoryEdge>> {
        Ok(vec![])
    }

    /// Traverse the graph starting from `root_ids` up to `max_depth` hops,
    /// returning the discovered subgraph (nodes + edges).
    async fn get_subgraph(&self, _root_ids: &[String], _max_depth: u32) -> Result<MemoryGraph> {
        Ok(MemoryGraph {
            nodes: vec![],
            edges: vec![],
        })
    }

    // ── Audit log methods (default no-op impls for backward compat) ─────

    /// Record a memory lifecycle event for audit purposes.
    async fn log_event(
        &self,
        _kind: MemoryEventKind,
        _memory_id: &str,
        _session_id: Option<&str>,
        _data: Option<&str>,
    ) -> Result<()> {
        Ok(())
    }

    /// Retrieve memory audit events, optionally filtered by memory ID.
    async fn get_events(
        &self,
        _memory_id: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<MemoryEvent>> {
        Ok(vec![])
    }

    /// Update the segment and importance of a memory (for recategorization).
    async fn update_segment(
        &self,
        _id: &str,
        _segment: MemorySegment,
        _importance: f32,
    ) -> Result<()> {
        Ok(())
    }

    // ── Consolidation run audit (default no-op) ────────────────────────────

    /// Record a completed consolidation run with full details.
    async fn log_consolidation_run(
        &self,
        _mode: &str,
        _memory_count: usize,
        _accepted: usize,
        _rejected: usize,
        _duration_ms: u64,
        _details: Option<&str>,
    ) -> Result<i64> {
        Ok(0)
    }
}
