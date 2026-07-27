//! In-memory mock implementations of `EmbeddingProvider` and `MemoryRepository`.

use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::domain::memory::{cosine_similarity, MemoryFragment, MemoryLifecycle};
use crate::user_data::ports::memory_repository::MemoryRepository;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Mock embedding provider — always returns a zero vector of `dims` length.
pub struct MockEmbeddingProvider {
    pub dims: usize,
}

impl MockEmbeddingProvider {
    pub fn new() -> Self {
        Self { dims: 384 }
    }
}

impl Default for MockEmbeddingProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0_f32; self.dims])
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

/// Mock memory repository — stores fragments in-memory.
///
/// `search_similar` mirrors the SQLite adapter: cosine over rows that actually
/// carry an embedding, falling back to `search_recent` when none do.
pub struct MockMemoryRepository {
    fragments: Arc<RwLock<Vec<MemoryFragment>>>,
}

impl MockMemoryRepository {
    pub fn new() -> Self {
        Self {
            fragments: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl Default for MockMemoryRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryRepository for MockMemoryRepository {
    async fn add(&self, fragment: MemoryFragment) -> Result<()> {
        self.fragments.write().await.push(fragment);
        Ok(())
    }

    async fn search_recent(
        &self,
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        let fragments = self.fragments.read().await;
        let mut results: Vec<MemoryFragment> = fragments
            .iter()
            .filter(|f| match profile_id {
                Some(pid) => f.profile_id.as_deref() == Some(pid),
                None => true,
            })
            .cloned()
            .collect();
        // newest first, then truncate
        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        results.truncate(limit);
        Ok(results)
    }

    async fn search_similar(
        &self,
        query_embedding: &[f32],
        profile_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        let scored: Vec<(f32, MemoryFragment)> = {
            let fragments = self.fragments.read().await;
            fragments
                .iter()
                .filter(|f| is_active(f))
                .filter(|f| match profile_id {
                    Some(pid) => f.profile_id.as_deref() == Some(pid),
                    None => true,
                })
                .filter_map(|f| {
                    let emb = f.embedding.as_ref()?;
                    Some((cosine_similarity(query_embedding, emb), f.clone()))
                })
                .collect()
        };

        // Same contract as the SQLite adapter: with nothing embedded at all,
        // degrade to recency rather than returning an empty result.
        if scored.is_empty() {
            return self.search_recent(profile_id, limit).await;
        }

        let mut scored = scored;
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, f)| f).collect())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.fragments.write().await.retain(|f| f.id != id);
        Ok(())
    }

    async fn search_unembedded(&self, limit: usize) -> Result<Vec<MemoryFragment>> {
        let fragments = self.fragments.read().await;
        Ok(fragments
            .iter()
            .filter(|f| f.embedding.is_none() && is_active(f))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn update_embedding(&self, id: &str, embedding: &[f32]) -> Result<()> {
        let mut fragments = self.fragments.write().await;
        if let Some(f) = fragments.iter_mut().find(|f| f.id == id) {
            f.embedding = Some(embedding.to_vec());
        }
        Ok(())
    }
}

/// Pre-lifecycle rows carry `None`, which the SQLite adapter treats as active.
fn is_active(fragment: &MemoryFragment) -> bool {
    !matches!(
        fragment.lifecycle,
        Some(MemoryLifecycle::Archived) | Some(MemoryLifecycle::Merged)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_embedding_returns_zero_vector() {
        let ep = MockEmbeddingProvider::new();
        let v = ep.embed("hello").await.unwrap();
        assert_eq!(v.len(), 384);
        assert!(v.iter().all(|&x| x == 0.0));
    }

    #[tokio::test]
    async fn mock_memory_add_and_search_recent() {
        let repo = MockMemoryRepository::new();
        let frag = MemoryFragment::from_chat(
            "id1".to_string(),
            Some("profile1".to_string()),
            None,
            "Hello world".to_string(),
        );
        repo.add(frag).await.unwrap();
        let results = repo.search_recent(Some("profile1"), 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Hello world");
    }

    #[tokio::test]
    async fn mock_memory_delete() {
        let repo = MockMemoryRepository::new();
        let frag = MemoryFragment::from_chat("del-id".to_string(), None, None, "temp".to_string());
        repo.add(frag).await.unwrap();
        repo.delete("del-id").await.unwrap();
        assert!(repo.search_recent(None, 10).await.unwrap().is_empty());
    }
}
