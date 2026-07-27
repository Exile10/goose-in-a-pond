//! Background memory extraction service.
//!
//! After each conversation turn, calls the `MemoryExtractor` port to pull
//! durable facts, deduplicates against existing memories, and stores them.
//! Runs asynchronously — must never block the SSE chat stream.

use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::domain::memory::{cosine_similarity, MemoryEventKind, MemoryFragment};
use crate::user_data::ports::memory_extractor::MemoryExtractor;
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::services::memory_relevance::{
    SEMANTIC_DEDUP_NEIGHBOURS, SEMANTIC_DEDUP_THRESHOLD,
};
use std::sync::Arc;
use tokio::sync::Mutex;

const MIN_MESSAGE_LEN: usize = 15;

pub struct MemoryExtractionService {
    last_run: Mutex<Option<chrono::DateTime<chrono::Utc>>>,
    /// Minimum seconds between extraction runs.
    interval_secs: i64,
    /// Optional embedder. When set, every stored fact carries an embedding and
    /// dedup gains a semantic pass on top of the lexical one. When unset (or on
    /// embed failure) facts are stored with `embedding: None`, exactly as
    /// before — extraction never fails because embedding did.
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
}

impl MemoryExtractionService {
    pub fn new(interval_secs: u32) -> Self {
        Self {
            last_run: Mutex::new(None),
            interval_secs: interval_secs as i64,
            embedding_provider: None,
        }
    }

    /// Attach an embedding provider so extracted facts are searchable by
    /// `search_similar` the moment they are written.
    pub fn with_embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Run extraction for a conversation turn.
    pub async fn run(
        &self,
        extractor: &dyn MemoryExtractor,
        repo: &dyn MemoryRepository,
        user_message: &str,
        assistant_response: &str,
        session_id: Option<&str>,
    ) {
        if user_message.len() < MIN_MESSAGE_LEN && assistant_response.len() < MIN_MESSAGE_LEN {
            return;
        }

        // Rate limit
        {
            let mut last = self.last_run.lock().await;
            let now = chrono::Utc::now();
            if let Some(prev) = *last {
                if (now - prev).num_seconds() < self.interval_secs {
                    tracing::debug!("[memory-extraction] rate-limited, skipping");
                    return;
                }
            }
            *last = Some(now);
        }

        // Fetch recent memories for dedup
        let existing: Vec<String> = repo
            .search_recent(None, 20)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.content.to_lowercase())
            .collect();

        // Extract facts
        let facts = match extractor
            .extract(user_message, assistant_response, &existing)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!("[memory-extraction] extraction failed: {e}");
                return;
            }
        };

        if facts.is_empty() {
            tracing::debug!("[memory-extraction] no facts extracted");
            return;
        }

        // Deduplicate and store
        let mut stored = 0;
        for fact in facts {
            // Skip empty/whitespace-only content
            if fact.content.trim().is_empty() {
                tracing::debug!("[memory-extraction] skipped empty fact");
                continue;
            }

            // Skip if content already exists (case-insensitive substring match)
            let lower = fact.content.to_lowercase();
            if existing
                .iter()
                .any(|e| e.contains(&lower) || lower.contains(e.as_str()))
            {
                tracing::debug!("[memory-extraction] dedup skipped: {:?}", fact.content);
                continue;
            }

            // Embedding is best-effort: a failure downgrades this fact to an
            // unembedded row (the startup backfill picks it up later), it never
            // aborts extraction.
            let embedding = match &self.embedding_provider {
                Some(provider) => match provider.embed(&fact.content).await {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::debug!(
                            "[memory-extraction] embed failed, storing unembedded: {e}"
                        );
                        None
                    }
                },
                None => None,
            };

            // Semantic dedup — catches paraphrases the lexical prefilter above
            // cannot. Rows without an embedding score 0.0, so the
            // search_similar -> search_recent fallback can never trigger a skip.
            if let Some(vector) = &embedding {
                if let Ok(neighbours) = repo
                    .search_similar(vector, None, SEMANTIC_DEDUP_NEIGHBOURS)
                    .await
                {
                    if let Some(dupe) = neighbours.iter().find(|n| {
                        n.embedding
                            .as_deref()
                            .map(|e| cosine_similarity(vector, e) >= SEMANTIC_DEDUP_THRESHOLD)
                            .unwrap_or(false)
                    }) {
                        tracing::debug!(
                            existing_id = %dupe.id,
                            "[memory-extraction] semantic dedup skipped: {:?}",
                            fact.content
                        );
                        continue;
                    }
                }
            }

            let id = uuid::Uuid::new_v4().to_string();
            let mut fragment = MemoryFragment::from_extraction(
                id.clone(),
                session_id.map(|s| s.to_string()),
                fact.content.clone(),
                fact.segment,
                fact.importance,
                fact.corrects.clone(),
            );
            fragment.embedding = embedding;

            if let Err(e) = repo.add(fragment).await {
                tracing::warn!("[memory-extraction] failed to store fact: {e}");
            } else {
                stored += 1;
                tracing::info!("[memory-extraction] stored: {:?}", fact.content);
                let _ = repo
                    .log_event(MemoryEventKind::Extracted, &id, session_id, None)
                    .await;
            }
        }

        if stored > 0 {
            tracing::info!("[memory-extraction] {stored} new memories from this turn");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::memory::{MemorySegment, MemoryTier};
    use crate::user_data::mocks::mock_memory::MockMemoryRepository;
    use crate::user_data::ports::memory_extractor::ExtractedFact;
    use anyhow::Result;
    use async_trait::async_trait;

    const TURN_USER: &str = "I keep my sourdough starter in the pantry";
    const TURN_ASSISTANT: &str = "Noted — the pantry it is.";

    /// Extractor that always yields one fixed fact.
    struct FixedExtractor(&'static str);

    #[async_trait]
    impl MemoryExtractor for FixedExtractor {
        async fn extract(&self, _u: &str, _a: &str, _e: &[String]) -> Result<Vec<ExtractedFact>> {
            Ok(vec![ExtractedFact {
                content: self.0.to_string(),
                segment: MemorySegment::Preference,
                importance: 0.7,
                tier: MemoryTier::Long,
                corrects: None,
            }])
        }
    }

    /// Embedding provider with a hand-picked vector per text, so tests control
    /// cosine similarity exactly.
    struct ScriptedEmbedder(Vec<(&'static str, Vec<f32>)>);

    #[async_trait]
    impl EmbeddingProvider for ScriptedEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            self.0
                .iter()
                .find(|(t, _)| *t == text)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| anyhow::anyhow!("no scripted embedding for {text:?}"))
        }
        fn dimensions(&self) -> usize {
            3
        }
    }

    /// Embedding provider that always fails.
    struct BrokenEmbedder;

    #[async_trait]
    impl EmbeddingProvider for BrokenEmbedder {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Err(anyhow::anyhow!("model not loaded"))
        }
        fn dimensions(&self) -> usize {
            3
        }
    }

    #[tokio::test]
    async fn stored_facts_carry_an_embedding_when_a_provider_is_wired() {
        let repo = MockMemoryRepository::new();
        let service = MemoryExtractionService::new(0).with_embedding_provider(Arc::new(
            ScriptedEmbedder(vec![("Starter lives in the pantry", vec![1.0, 0.0, 0.0])]),
        ));

        service
            .run(
                &FixedExtractor("Starter lives in the pantry"),
                &repo,
                TURN_USER,
                TURN_ASSISTANT,
                None,
            )
            .await;

        let stored = repo.search_recent(None, 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].embedding, Some(vec![1.0, 0.0, 0.0]));
    }

    #[tokio::test]
    async fn a_paraphrase_above_the_cosine_threshold_is_skipped() {
        let repo = MockMemoryRepository::new();
        // Existing memory, already embedded.
        let mut existing = MemoryFragment::from_extraction(
            "existing".to_string(),
            None,
            "The sourdough starter is kept in the pantry".to_string(),
            MemorySegment::Preference,
            0.7,
            None,
        );
        existing.embedding = Some(vec![1.0, 0.0, 0.0]);
        repo.add(existing).await.unwrap();

        // A lexically distinct paraphrase that embeds almost identically.
        let paraphrase = "Starter lives in the pantry";
        let service = MemoryExtractionService::new(0).with_embedding_provider(Arc::new(
            ScriptedEmbedder(vec![(paraphrase, vec![0.99, 0.1, 0.0])]),
        ));

        service
            .run(
                &FixedExtractor(paraphrase),
                &repo,
                TURN_USER,
                TURN_ASSISTANT,
                None,
            )
            .await;

        let stored = repo.search_recent(None, 10).await.unwrap();
        assert_eq!(
            stored.len(),
            1,
            "the paraphrase should not have been stored"
        );
        assert_eq!(stored[0].id, "existing");
    }

    #[tokio::test]
    async fn an_unrelated_fact_below_the_threshold_is_stored() {
        let repo = MockMemoryRepository::new();
        let mut existing = MemoryFragment::from_extraction(
            "existing".to_string(),
            None,
            "The sourdough starter is kept in the pantry".to_string(),
            MemorySegment::Preference,
            0.7,
            None,
        );
        existing.embedding = Some(vec![1.0, 0.0, 0.0]);
        repo.add(existing).await.unwrap();

        let unrelated = "The greenhouse pump runs at dawn";
        let service = MemoryExtractionService::new(0).with_embedding_provider(Arc::new(
            ScriptedEmbedder(vec![(unrelated, vec![0.0, 1.0, 0.0])]),
        ));

        service
            .run(
                &FixedExtractor(unrelated),
                &repo,
                TURN_USER,
                TURN_ASSISTANT,
                None,
            )
            .await;

        assert_eq!(repo.search_recent(None, 10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_embedding_failure_still_stores_the_fact_unembedded() {
        let repo = MockMemoryRepository::new();
        let service =
            MemoryExtractionService::new(0).with_embedding_provider(Arc::new(BrokenEmbedder));

        service
            .run(
                &FixedExtractor("Starter lives in the pantry"),
                &repo,
                TURN_USER,
                TURN_ASSISTANT,
                None,
            )
            .await;

        let stored = repo.search_recent(None, 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].embedding.is_none());
    }

    #[tokio::test]
    async fn without_a_provider_behaviour_is_unchanged() {
        let repo = MockMemoryRepository::new();
        let service = MemoryExtractionService::new(0);

        service
            .run(
                &FixedExtractor("Starter lives in the pantry"),
                &repo,
                TURN_USER,
                TURN_ASSISTANT,
                None,
            )
            .await;

        let stored = repo.search_recent(None, 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].embedding.is_none());
    }
}
