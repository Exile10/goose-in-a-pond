//! Background memory extraction service.
//!
//! After each conversation turn, calls the `MemoryExtractor` port to pull
//! durable facts, deduplicates against existing memories, and stores them.
//! Runs asynchronously — must never block the SSE chat stream.
//!
//! This service is the *write gate* for extracted memory: whatever an extractor
//! adapter hands over, nothing reaches the store without passing
//! [`fact_defect`] and the dedup pass. Adapters may prompt their model as well
//! as they like, but they are not trusted to be the only enforcement — a 4B
//! model reliably ignores part of any instruction it is given.

use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::domain::memory::{
    cosine_similarity, fact_defect, is_captured_request, normalise_fact_content, MemoryEventKind,
    MemoryFragment, MemorySegment,
};
use crate::user_data::ports::memory_extractor::MemoryExtractor;
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::services::memory_relevance::{
    is_duplicate_content, DEDUP_RECENT_WINDOW, SEMANTIC_DEDUP_NEIGHBOURS, SEMANTIC_DEDUP_THRESHOLD,
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

        // Fetch recent memories for dedup. Grows as this run stores facts, so
        // two near-identical facts in one turn cannot both land.
        let mut existing: Vec<String> = repo
            .search_recent(None, DEDUP_RECENT_WINDOW)
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
            let content = normalise_fact_content(&fact.content);

            // Quality gate. A defective fact is dropped, never repaired: the
            // rewrite a mechanical fix would need (conjugating "I like" into
            // "The user likes", inventing the referent of "the latter") is
            // exactly the judgement we do not have here, and a wrong repair
            // outlives the conversation that could have corrected it.
            if let Some(defect) = fact_defect(&content) {
                tracing::debug!(
                    defect = %defect,
                    "[memory-extraction] rejected fact: {content:?}"
                );
                continue;
            }

            // A correction is the one thing dedup must never swallow. It
            // restates the fact it fixes, in almost the same words, which is
            // exactly what both measures score as a duplicate — and dropping it
            // leaves the *stale* row standing, so the store ends up asserting
            // the thing the user just took the trouble to deny. Keep it and let
            // consolidation supersede the old row, which is the one path that
            // knows which of the two won.
            let is_correction =
                fact.segment == MemorySegment::Correction || fact.corrects.is_some();

            if !is_correction && existing.iter().any(|e| is_duplicate_content(e, &content)) {
                tracing::debug!("[memory-extraction] dedup skipped: {content:?}");
                continue;
            }

            // A one-off request the assistant already carried out is not an
            // ongoing project. Filed as Context it still informs the next few
            // turns, then decays out (short tier) instead of sitting in the
            // Project segment forever crowding out real commitments.
            let (segment, importance) =
                if fact.segment == MemorySegment::Project && is_captured_request(&content) {
                    tracing::debug!(
                        "[memory-extraction] reclassified captured request as context: {content:?}"
                    );
                    (
                        MemorySegment::Context,
                        MemorySegment::Context.default_importance(),
                    )
                } else {
                    (fact.segment.clone(), fact.importance)
                };

            // Embedding is best-effort: a failure downgrades this fact to an
            // unembedded row (the startup backfill picks it up later), it never
            // aborts extraction.
            let embedding = match &self.embedding_provider {
                Some(provider) => match provider.embed(&content).await {
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
            // Corrections are exempt here for the same reason as above: a
            // correction embeds close to the claim it overturns by design.
            let semantic_candidate = if is_correction {
                None
            } else {
                embedding.as_deref()
            };
            if let Some(vector) = semantic_candidate {
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
                            "[memory-extraction] semantic dedup skipped: {content:?}"
                        );
                        continue;
                    }
                }
            }

            let id = uuid::Uuid::new_v4().to_string();
            let mut fragment = MemoryFragment::from_extraction(
                id.clone(),
                session_id.map(|s| s.to_string()),
                content.clone(),
                segment,
                importance,
                fact.corrects.clone(),
            );
            fragment.embedding = embedding;

            if let Err(e) = repo.add(fragment).await {
                tracing::warn!("[memory-extraction] failed to store fact: {e}");
            } else {
                stored += 1;
                tracing::info!("[memory-extraction] stored: {content:?}");
                existing.push(content.to_lowercase());
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

    /// Extractor that yields a scripted list of facts, so a test can drive
    /// segment and multi-fact behaviour.
    struct ScriptedExtractor(Vec<ExtractedFact>);

    impl ScriptedExtractor {
        fn one(content: &str, segment: MemorySegment) -> Self {
            Self(vec![ExtractedFact {
                content: content.to_string(),
                importance: segment.default_importance(),
                tier: segment.default_tier(),
                segment,
                corrects: None,
            }])
        }
    }

    #[async_trait]
    impl MemoryExtractor for ScriptedExtractor {
        async fn extract(&self, _u: &str, _a: &str, _e: &[String]) -> Result<Vec<ExtractedFact>> {
            Ok(self.0.clone())
        }
    }

    async fn run_once(extractor: &dyn MemoryExtractor, repo: &MockMemoryRepository) {
        MemoryExtractionService::new(0)
            .run(extractor, repo, TURN_USER, TURN_ASSISTANT, None)
            .await;
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

    // ── quality gate ────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_fact_the_gate_rejects_never_reaches_the_store() {
        // Both rows are verbatim from a real memory store.
        for junk in [
            "The user's mother lives in the latter city.",
            "My mom's name is Florence and she lives in the latter city",
        ] {
            let repo = MockMemoryRepository::new();
            run_once(&FixedExtractor(junk), &repo).await;
            assert!(
                repo.search_recent(None, 10).await.unwrap().is_empty(),
                "stored junk: {junk:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_captured_request_is_filed_as_short_lived_context() {
        let repo = MockMemoryRepository::new();
        run_once(
            &ScriptedExtractor::one(
                "Active Project: Set a reminder to water the plants every evening at 6 PM",
                MemorySegment::Project,
            ),
            &repo,
        )
        .await;

        let stored = repo.search_recent(None, 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].segment, Some(MemorySegment::Context));
        assert_eq!(stored[0].tier, Some(MemoryTier::Short));
        assert_eq!(stored[0].importance, Some(0.3));
        assert_eq!(
            stored[0].content, "Set a reminder to water the plants every evening at 6 PM",
            "the invented label prefix should be gone"
        );
    }

    #[tokio::test]
    async fn a_genuine_project_keeps_its_segment() {
        let repo = MockMemoryRepository::new();
        run_once(
            &ScriptedExtractor::one(
                "The user is building a smart-home dashboard for the Jetson.",
                MemorySegment::Project,
            ),
            &repo,
        )
        .await;

        let stored = repo.search_recent(None, 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].segment, Some(MemorySegment::Project));
    }

    #[tokio::test]
    async fn a_reworded_duplicate_is_skipped_with_no_embeddings_at_all() {
        // The degraded path: embedding_provider = "none", so the cosine pass is
        // inert and the reworded copy shares no substring with the original.
        let repo = MockMemoryRepository::new();
        repo.add(MemoryFragment::from_extraction(
            "existing".to_string(),
            None,
            "The user's mother's name is Florence.".to_string(),
            MemorySegment::Relationship,
            0.7,
            None,
        ))
        .await
        .unwrap();

        run_once(
            &FixedExtractor("Florence is the name of the user's mother."),
            &repo,
        )
        .await;

        let stored = repo.search_recent(None, 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, "existing");
    }

    #[tokio::test]
    async fn two_rewordings_of_one_fact_in_a_single_turn_store_once() {
        let repo = MockMemoryRepository::new();
        let segment = MemorySegment::Relationship;
        let extractor = ScriptedExtractor(vec![
            ExtractedFact {
                content: "The user's mother's name is Florence.".to_string(),
                segment: segment.clone(),
                importance: 0.7,
                tier: segment.default_tier(),
                corrects: None,
            },
            ExtractedFact {
                content: "Florence is the name of the user's mother.".to_string(),
                segment: segment.clone(),
                importance: 0.7,
                tier: segment.default_tier(),
                corrects: None,
            },
        ]);

        run_once(&extractor, &repo).await;
        assert_eq!(repo.search_recent(None, 10).await.unwrap().len(), 1);
    }

    // ── corrections survive dedup ───────────────────────────────────────

    /// Store one fact, then run the extractor with `correction` and see what
    /// is left. Returns the contents in insertion order.
    async fn store_then_extract(existing: &str, correction: ExtractedFact) -> Vec<String> {
        let repo = MockMemoryRepository::new();
        repo.add(MemoryFragment::from_extraction(
            "stale".to_string(),
            None,
            existing.to_string(),
            MemorySegment::Preference,
            0.7,
            None,
        ))
        .await
        .unwrap();

        run_once(&ScriptedExtractor(vec![correction]), &repo).await;

        let mut rows = repo.search_recent(None, 10).await.unwrap();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        rows.into_iter().map(|f| f.content).collect()
    }

    #[tokio::test]
    async fn a_reversed_correction_does_not_lose_to_the_stale_row() {
        // The regression: both sides reduce to the identical token set, so the
        // correction was dropped and only the row it contradicts survived.
        let stale = "The user prefers dark mode over light mode";
        let fixed = "The user prefers light mode over dark mode";
        let stored = store_then_extract(
            stale,
            ExtractedFact {
                content: fixed.to_string(),
                segment: MemorySegment::Correction,
                importance: 0.9,
                tier: MemoryTier::Long,
                corrects: Some(stale.to_string()),
            },
        )
        .await;
        assert_eq!(stored.len(), 2, "the correction was dropped: {stored:?}");
        assert!(stored.iter().any(|c| c == fixed));
    }

    #[tokio::test]
    async fn a_correction_survives_even_when_it_is_a_lexical_duplicate() {
        // Same order, same words plus one — the token measure calls this a
        // duplicate (Jaccard 0.75, containment 1.00). Only the Correction
        // exemption keeps it, which is what isolates it from the order fix.
        let stale = "The user's mother's name is Florence.";
        let fixed = "The user's mother's name is Florence Atieno.";
        assert!(
            is_duplicate_content(stale, fixed),
            "test is only meaningful if the measure still fires"
        );
        let stored = store_then_extract(
            stale,
            ExtractedFact {
                content: fixed.to_string(),
                segment: MemorySegment::Relationship,
                importance: 0.7,
                tier: MemoryTier::Long,
                // Segment is not Correction — the `corrects` field alone must
                // be enough, matching MemoryFragment::is_correction().
                corrects: Some(stale.to_string()),
            },
        )
        .await;
        assert_eq!(stored.len(), 2, "the correction was dropped: {stored:?}");
    }

    #[tokio::test]
    async fn a_plain_restatement_is_still_deduplicated() {
        // The exemption is scoped to corrections; an ordinary reword still goes.
        let stored = store_then_extract(
            "The user's mother's name is Florence.",
            ExtractedFact {
                content: "Florence is the name of the user's mother.".to_string(),
                segment: MemorySegment::Relationship,
                importance: 0.7,
                tier: MemoryTier::Long,
                corrects: None,
            },
        )
        .await;
        assert_eq!(stored.len(), 1);
    }

    #[tokio::test]
    async fn a_correction_survives_the_semantic_pass_too() {
        let repo = MockMemoryRepository::new();
        let stale = "The user prefers dark mode over light mode";
        let fixed = "The user prefers light mode over dark mode";
        let mut existing = MemoryFragment::from_extraction(
            "stale".to_string(),
            None,
            stale.to_string(),
            MemorySegment::Preference,
            0.7,
            None,
        );
        existing.embedding = Some(vec![1.0, 0.0, 0.0]);
        repo.add(existing).await.unwrap();

        // A correction embeds almost on top of the claim it overturns.
        let service = MemoryExtractionService::new(0).with_embedding_provider(Arc::new(
            ScriptedEmbedder(vec![(fixed, vec![0.99, 0.1, 0.0])]),
        ));
        service
            .run(
                &ScriptedExtractor(vec![ExtractedFact {
                    content: fixed.to_string(),
                    segment: MemorySegment::Correction,
                    importance: 0.9,
                    tier: MemoryTier::Long,
                    corrects: Some(stale.to_string()),
                }]),
                &repo,
                TURN_USER,
                TURN_ASSISTANT,
                None,
            )
            .await;

        assert_eq!(repo.search_recent(None, 10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn distinct_facts_in_one_turn_are_all_kept() {
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor(vec![
            ExtractedFact {
                content: "The user's mother's name is Florence.".to_string(),
                segment: MemorySegment::Relationship,
                importance: 0.7,
                tier: MemoryTier::Long,
                corrects: None,
            },
            ExtractedFact {
                content: "The user's mother lives in Kisumu.".to_string(),
                segment: MemorySegment::Relationship,
                importance: 0.7,
                tier: MemoryTier::Long,
                corrects: None,
            },
        ]);

        run_once(&extractor, &repo).await;
        assert_eq!(repo.search_recent(None, 10).await.unwrap().len(), 2);
    }
}
