//! Memory relevance — retrieval-side scoring shared by the injection path and
//! the extraction pipeline.
//!
//! Three concerns live here, all pure so they stay testable in the fast-crate
//! pass:
//!
//! 1. [`keyword_terms`] — the stopword-filtered fallback used when no
//!    embedding provider is wired (or embedding the turn's message failed).
//! 2. [`relevance_score`] / [`rank_by_relevance`] — the blend of semantic
//!    similarity, importance, and recency that decides which memories survive
//!    the per-turn token budget.
//! 3. [`SEMANTIC_DEDUP_THRESHOLD`] — the cosine floor above which a newly
//!    extracted fact is considered a paraphrase of one already stored.

use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::domain::memory::MemoryFragment;
use crate::user_data::ports::memory_repository::MemoryRepository;
use chrono::{DateTime, Utc};

// ── Keyword fallback ─────────────────────────────────────────────────────────

/// Words carrying no retrieval signal. Kept small and English-only on purpose:
/// this list is only reached when semantic search is unavailable, and a bigger
/// list is a bigger chance of dropping a genuinely topical short word.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "your", "yours", "all", "any", "can", "had",
    "has", "have", "her", "his", "its", "our", "out", "was", "were", "who", "whom", "will", "with",
    "what", "when", "where", "which", "why", "how", "this", "that", "these", "those", "there",
    "their", "them", "then", "than", "they", "from", "into", "onto", "over", "under", "about",
    "just", "like", "some", "such", "only", "very", "much", "more", "most", "also", "been",
    "being", "does", "did", "done", "doing", "should", "would", "could", "please", "thanks",
    "thank", "let", "get", "got", "make", "made", "want", "need", "know", "tell", "say", "said",
    "one", "two", "now", "new", "old", "yes", "sure", "okay", "hey", "hi", "hello",
];

/// Minimum keyword length. Shorter tokens are almost always function words and
/// match far too broadly through a SQL `LIKE %kw%`.
const MIN_KEYWORD_LEN: usize = 3;

/// Derive fallback search keywords from a user message.
///
/// Lowercases, strips surrounding punctuation, drops tokens shorter than
/// [`MIN_KEYWORD_LEN`] and known stopwords, and de-duplicates while preserving
/// first-seen order.
pub fn keyword_terms(message: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for raw in message.split_whitespace() {
        let word = raw
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if word.len() < MIN_KEYWORD_LEN || STOPWORDS.contains(&word.as_str()) {
            continue;
        }
        if !seen.iter().any(|w| w == &word) {
            seen.push(word);
        }
    }
    seen
}

// ── Relevance blend ──────────────────────────────────────────────────────────

/// Weight of semantic similarity in the injection score.
pub const SIMILARITY_WEIGHT: f32 = 0.5;
/// Weight of the stored importance score.
pub const IMPORTANCE_WEIGHT: f32 = 0.3;
/// Weight of the recency term.
pub const RECENCY_WEIGHT: f32 = 0.2;

/// Importance assumed for fragments written before importance was recorded.
const DEFAULT_IMPORTANCE: f32 = 0.5;

/// Half-life of the recency term, in days. A memory touched today scores 1.0;
/// one untouched for two weeks scores 0.5. Deliberately short: recency is the
/// tiebreaker between comparably relevant memories, not a retention policy
/// (that is `memory_cleanup`'s decay).
pub const RECENCY_HALF_LIFE_DAYS: f32 = 14.0;

/// Recency term in `[0, 1]` for a fragment, measured from its last access if
/// it has one and its creation time otherwise.
pub fn recency_score(fragment: &MemoryFragment, now: DateTime<Utc>) -> f32 {
    let reference = fragment.last_accessed_at.unwrap_or(fragment.created_at);
    let days = (now - reference).num_seconds() as f32 / 86_400.0;
    if days <= 0.0 {
        return 1.0;
    }
    0.5_f32.powf(days / RECENCY_HALF_LIFE_DAYS)
}

/// Blended injection score for one candidate memory.
///
/// `similarity` is `None` for candidates that arrived by recency alone; they
/// forfeit the similarity term rather than being assigned a neutral value, so a
/// topical semantic hit can displace a standing high-importance identity
/// memory instead of always losing to it.
pub fn relevance_score(
    fragment: &MemoryFragment,
    similarity: Option<f32>,
    now: DateTime<Utc>,
) -> f32 {
    let sim = similarity.unwrap_or(0.0).clamp(0.0, 1.0);
    let importance = fragment
        .importance
        .unwrap_or(DEFAULT_IMPORTANCE)
        .clamp(0.0, 1.0);
    let recency = recency_score(fragment, now);
    SIMILARITY_WEIGHT * sim + IMPORTANCE_WEIGHT * importance + RECENCY_WEIGHT * recency
}

/// Sort injection candidates best-first by [`relevance_score`].
///
/// Ties break on fragment id so the ordering (and therefore the prompt, and
/// therefore the KV prefix beyond it) is reproducible for identical inputs.
pub fn rank_by_relevance(candidates: &mut [(MemoryFragment, Option<f32>)], now: DateTime<Utc>) {
    candidates.sort_by(|a, b| {
        let sa = relevance_score(&a.0, a.1, now);
        let sb = relevance_score(&b.0, b.1, now);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
}

// ── Semantic dedup ───────────────────────────────────────────────────────────

/// Cosine floor above which a new fact is treated as a paraphrase of an
/// existing memory and dropped. Tuned high: a false skip silently loses a fact,
/// while a false keep is only a redundant row that consolidation can merge.
pub const SEMANTIC_DEDUP_THRESHOLD: f32 = 0.92;

/// How many nearest neighbours to inspect when checking for a paraphrase.
pub const SEMANTIC_DEDUP_NEIGHBOURS: usize = 5;

// ── Embedding backfill ───────────────────────────────────────────────────────

/// Rows embedded per backfill batch.
///
/// Also bounds how long a concurrent per-turn retrieval embed can queue behind
/// the backfill: the fastembed adapter serialises on one model mutex, so a turn
/// arriving mid-batch waits for at most this many short embeds.
pub const BACKFILL_BATCH_SIZE: usize = 32;

/// Pause between backfill batches. The backfill competes with inference for CPU
/// on a Jetson, so it yields between batches rather than running flat out.
pub const BACKFILL_BATCH_PAUSE_MS: u64 = 250;

/// Embed every active memory that has no stored embedding yet, in batches.
///
/// Extraction historically stored `embedding: None`, so without this pass
/// `search_similar` sees only the handful of rows written by the `save_memory`
/// MCP tool and silently ignores the rest of the store.
///
/// Best-effort throughout: a row that fails to embed is left for the next run.
/// Returns the number of rows embedded.
pub async fn run_backfill(
    repo: &dyn MemoryRepository,
    embedder: &dyn EmbeddingProvider,
    batch_size: usize,
    pause_ms: u64,
) -> usize {
    let mut embedded = 0usize;
    loop {
        let batch = match repo.search_unembedded(batch_size).await {
            Ok(rows) if rows.is_empty() => break,
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("[memory-backfill] fetch failed: {e}");
                break;
            }
        };

        let mut progressed = false;
        for fragment in &batch {
            match embedder.embed(&fragment.content).await {
                Ok(vector) => match repo.update_embedding(&fragment.id, &vector).await {
                    Ok(()) => {
                        embedded += 1;
                        progressed = true;
                    }
                    Err(e) => {
                        tracing::warn!("[memory-backfill] store failed for {}: {e}", fragment.id)
                    }
                },
                Err(e) => tracing::warn!("[memory-backfill] embed failed for {}: {e}", fragment.id),
            }
        }

        // Every row in the batch failed, so the same rows would come back
        // forever — stop instead of spinning.
        if !progressed {
            tracing::warn!("[memory-backfill] no progress in a batch — stopping");
            break;
        }

        if pause_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(pause_ms)).await;
        }
    }

    if embedded > 0 {
        tracing::info!("[memory-backfill] embedded {embedded} previously unembedded memories");
    }
    embedded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::memory::{MemoryLifecycle, MemorySegment};
    use crate::user_data::mocks::mock_memory::MockMemoryRepository;
    use chrono::Duration;

    fn fragment(id: &str, importance: f32, age_days: i64) -> MemoryFragment {
        let mut f = MemoryFragment::from_extraction(
            id.to_string(),
            None,
            format!("content of {id}"),
            MemorySegment::Knowledge,
            importance,
            None,
        );
        f.created_at = Utc::now() - Duration::days(age_days);
        f
    }

    // ── keyword fallback ────────────────────────────────────────────────

    #[test]
    fn keyword_terms_drops_stopwords_and_short_tokens() {
        let terms = keyword_terms("What is the status of my greenhouse irrigation pump?");
        assert!(!terms.contains(&"the".to_string()));
        assert!(!terms.contains(&"what".to_string()));
        assert!(!terms.contains(&"is".to_string()));
        assert!(!terms.contains(&"my".to_string()));
        assert!(terms.contains(&"greenhouse".to_string()));
        assert!(terms.contains(&"irrigation".to_string()));
        assert!(terms.contains(&"pump".to_string()));
        assert!(terms.contains(&"status".to_string()));
    }

    #[test]
    fn keyword_terms_strips_punctuation_and_dedupes() {
        let terms = keyword_terms("Pump, pump... PUMP!");
        assert_eq!(terms, vec!["pump".to_string()]);
    }

    #[test]
    fn keyword_terms_can_return_nothing_for_a_pure_stopword_message() {
        assert!(keyword_terms("and then you know, they said what?").is_empty());
    }

    // ── ranking blend ───────────────────────────────────────────────────

    #[test]
    fn recency_score_halves_every_half_life() {
        let now = Utc::now();
        let fresh = fragment("fresh", 0.5, 0);
        let two_weeks = fragment("two-weeks", 0.5, RECENCY_HALF_LIFE_DAYS as i64);
        assert!((recency_score(&fresh, now) - 1.0).abs() < 0.01);
        assert!((recency_score(&two_weeks, now) - 0.5).abs() < 0.02);
    }

    #[test]
    fn last_access_beats_creation_time_for_recency() {
        let now = Utc::now();
        let mut old_but_used = fragment("used", 0.5, 90);
        old_but_used.last_accessed_at = Some(now);
        assert!((recency_score(&old_but_used, now) - 1.0).abs() < 0.01);
    }

    #[test]
    fn topical_similarity_outranks_standing_identity_memory() {
        let now = Utc::now();
        // The standing identity block: maximum importance, recent, but no
        // semantic relationship to this turn.
        let mut identity = fragment("identity", 1.0, 0);
        identity.segment = Some(MemorySegment::Identity);
        // An old, middling-importance memory that is actually about the topic.
        let topical = fragment("topical", 0.5, 60);

        let mut candidates = vec![(identity, None), (topical, Some(0.88))];
        rank_by_relevance(&mut candidates, now);
        assert_eq!(candidates[0].0.id, "topical");
    }

    #[test]
    fn importance_still_decides_when_similarity_is_equal() {
        let now = Utc::now();
        let low = fragment("low", 0.2, 0);
        let high = fragment("high", 0.9, 0);
        let mut candidates = vec![(low, Some(0.5)), (high, Some(0.5))];
        rank_by_relevance(&mut candidates, now);
        assert_eq!(candidates[0].0.id, "high");
    }

    #[test]
    fn ranking_is_deterministic_for_identical_scores() {
        let now = Utc::now();
        let a = fragment("aaa", 0.5, 0);
        let b = fragment("bbb", 0.5, 0);
        let mut forward = vec![(a.clone(), Some(0.4)), (b.clone(), Some(0.4))];
        let mut reversed = vec![(b, Some(0.4)), (a, Some(0.4))];
        rank_by_relevance(&mut forward, now);
        rank_by_relevance(&mut reversed, now);
        assert_eq!(forward[0].0.id, "aaa");
        assert_eq!(reversed[0].0.id, "aaa");
    }

    #[test]
    fn missing_similarity_scores_as_zero_not_as_neutral() {
        let now = Utc::now();
        let f = fragment("f", 0.6, 0);
        let without = relevance_score(&f, None, now);
        let with_zero = relevance_score(&f, Some(0.0), now);
        assert!((without - with_zero).abs() < f32::EPSILON);
    }

    // ── backfill ────────────────────────────────────────────────────────

    /// Embedding provider returning a fixed non-zero vector, so backfilled rows
    /// are distinguishable from the all-zero mock.
    struct FixedEmbedder(Vec<f32>);

    #[async_trait::async_trait]
    impl EmbeddingProvider for FixedEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(self.0.clone())
        }
        fn dimensions(&self) -> usize {
            self.0.len()
        }
    }

    #[tokio::test]
    async fn backfill_embeds_every_unembedded_active_row() {
        let repo = MockMemoryRepository::new();
        for i in 0..5 {
            repo.add(fragment(&format!("m{i}"), 0.5, 0)).await.unwrap();
        }
        let mut already = fragment("already", 0.5, 0);
        already.embedding = Some(vec![9.0, 9.0]);
        repo.add(already).await.unwrap();

        let embedder = FixedEmbedder(vec![1.0, 0.0]);
        let count = run_backfill(&repo, &embedder, 2, 0).await;

        assert_eq!(count, 5);
        assert!(repo.search_unembedded(10).await.unwrap().is_empty());
        // The pre-embedded row is left exactly as it was.
        let stored = repo.search_recent(None, 10).await.unwrap();
        let untouched = stored.iter().find(|f| f.id == "already").unwrap();
        assert_eq!(untouched.embedding, Some(vec![9.0, 9.0]));
    }

    #[tokio::test]
    async fn backfill_skips_archived_rows() {
        let repo = MockMemoryRepository::new();
        let mut archived = fragment("archived", 0.5, 0);
        archived.lifecycle = Some(MemoryLifecycle::Archived);
        repo.add(archived).await.unwrap();
        repo.add(fragment("active", 0.5, 0)).await.unwrap();

        let embedded = run_backfill(&repo, &FixedEmbedder(vec![1.0, 0.0]), 8, 0).await;
        assert_eq!(embedded, 1);
    }

    #[tokio::test]
    async fn backfill_on_an_empty_store_is_a_no_op() {
        let repo = MockMemoryRepository::new();
        assert_eq!(
            run_backfill(&repo, &FixedEmbedder(vec![1.0]), 8, 0).await,
            0
        );
    }
}
