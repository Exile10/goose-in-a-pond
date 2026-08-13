//! Keeping the personal-context index honest (phase D).
//!
//! The index is derived data, and derived data drifts. Four things go wrong on
//! their own, and this is the one pass that repairs all of them:
//!
//! * **Rows that were never indexed** — a memory embedded before the index
//!   existed, or written while the index file was missing.
//! * **Rows embedded by a different model** — someone changed
//!   `embedding_provider`. Those vectors are unusable and, until they are
//!   replaced, invisible to retrieval by design.
//! * **Summaries that have been rewritten** — the vector describes an older
//!   conversation while still being present.
//! * **Orphans** — a source row deleted while the index was not looking. Under
//!   WAL a cross-file delete cannot be atomic, so this is expected rather than
//!   exceptional.
//!
//! # Why this is a sweep and not a queue
//!
//! Everything above is discovered by a `LEFT JOIN` against the live stores, so
//! the pass is crash-safe, restartable, and self-healing: interrupt it anywhere
//! and the next run recomputes exactly what is still outstanding. A durable
//! queue fails the other way — a dropped entry is a row that is never searchable
//! again, with nothing left to notice.
//!
//! # Why it yields
//!
//! On a Jetson the embedder competes with the chat model for CPU. Every step is
//! batched with a pause, and the whole pass is cancellable, so a member's turn
//! always wins.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::context::summary_indexing::{run_summary_indexing, SUMMARY_BATCH_PAUSE_MS};
use crate::context::vector_index::{Corpus, VectorIndex};
use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::ports::session_storage::SessionStorage;

/// What one maintenance pass did. Reported so "the index is quietly incomplete"
/// is a number somebody can read rather than something inferred from bad answers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaintenanceReport {
    /// Existing vectors taught to the index without re-embedding.
    pub adopted: u64,
    /// Summaries embedded (the only step that costs inference).
    pub summaries_indexed: usize,
    /// Index rows whose source row is gone.
    pub orphans_pruned: u64,
    /// Rows still lacking a usable vector when the pass finished.
    pub still_missing: u64,
    /// Vectors from another model, still awaiting re-embedding.
    pub mismatched: u64,
}

/// Run one full maintenance pass. Never fails: every step is best-effort and
/// logged, because a repair pass that aborts the process it runs in is worse
/// than one that leaves work for the next run.
pub async fn run_index_maintenance(
    index: &Arc<dyn VectorIndex>,
    storage: &dyn SessionStorage,
    embedder: &dyn EmbeddingProvider,
    cancel: &CancellationToken,
) -> MaintenanceReport {
    let mut report = MaintenanceReport::default();
    let model_id = embedder.model_id();
    let dims = embedder.dimensions();

    // 1. Adoption first, because it is free. Any vector that already exists in a
    //    source table is copied in pure SQL, with no inference at all -- so the
    //    expensive steps below have less to do.
    for corpus in [Corpus::Memory, Corpus::Context] {
        if cancel.is_cancelled() {
            return report;
        }
        match index.backfill_from_source(corpus, &model_id, dims).await {
            Ok(n) => report.adopted += n,
            Err(e) => tracing::warn!(corpus = corpus.as_str(), "index adoption failed: {e:#}"),
        }
    }

    // 2. Summaries: the only corpus with no vector of its own, so the only step
    //    that spends the device's scarce resource.
    if !cancel.is_cancelled() {
        report.summaries_indexed = run_summary_indexing(
            storage,
            embedder,
            index,
            cancel,
            crate::context::summary_indexing::SUMMARY_BATCH_SIZE,
            SUMMARY_BATCH_PAUSE_MS,
        )
        .await;
    }

    // 3. Orphans. Deliberately AFTER the writes: pruning first would delete rows
    //    that step 1 is about to legitimately re-create, doing the same work
    //    twice on every pass.
    if !cancel.is_cancelled() {
        match index.prune_orphans().await {
            Ok(n) => report.orphans_pruned = n,
            Err(e) => tracing::warn!("orphan prune failed: {e:#}"),
        }
    }

    // 4. Report what is still wrong. A model change must be LOUD: every stored
    //    vector from the old model is meaningless while still scoring plausibly,
    //    and re-embedding a household can take hours on a Jetson. "Retrieval
    //    quietly got worse" is undiagnosable, so it gets a WARN with the count.
    match index.health(&model_id).await {
        Ok(h) => {
            report.still_missing = h.missing;
            report.mismatched = h.mismatched;
            if h.mismatched > 0 {
                tracing::warn!(
                    model_id = %model_id,
                    mismatched = h.mismatched,
                    "vectors from a different embedding model are present and unusable; \
                     retrieval excludes them until they are re-embedded"
                );
            }
            if h.missing > 0 {
                tracing::info!(missing = h.missing, "rows still awaiting a vector");
            }
        }
        Err(e) => tracing::warn!("index health read failed: {e:#}"),
    }

    if report.adopted > 0 || report.summaries_indexed > 0 || report.orphans_pruned > 0 {
        tracing::info!(
            adopted = report.adopted,
            summaries = report.summaries_indexed,
            orphans_pruned = report.orphans_pruned,
            "personal-context index maintenance pass complete"
        );
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::vector_index::{IndexHealth, ResolvedHit, VectorEntry, VectorHit};
    use crate::user_data::domain::profile::ProfileScope;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Default)]
    struct SpyIndex {
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl VectorIndex for SpyIndex {
        async fn upsert(&self, _e: &VectorEntry) -> Result<()> {
            Ok(())
        }
        async fn remove(&self, _c: Corpus, _r: &str) -> Result<()> {
            Ok(())
        }
        async fn get(&self, _c: Corpus, _r: &str) -> Result<Option<VectorEntry>> {
            Ok(None)
        }
        async fn search(
            &self,
            _q: &[f32],
            _m: &str,
            _s: &ProfileScope,
            _l: usize,
        ) -> Result<Vec<VectorHit>> {
            Ok(vec![])
        }
        async fn search_resolved(
            &self,
            _q: &[f32],
            _m: &str,
            _s: &ProfileScope,
            _l: usize,
        ) -> Result<Vec<ResolvedHit>> {
            Ok(vec![])
        }
        async fn needs_embedding(&self, _c: Corpus, _m: &str, _l: usize) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn backfill_from_source(&self, c: Corpus, _m: &str, _d: usize) -> Result<u64> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("adopt:{}", c.as_str()));
            Ok(2)
        }
        async fn prune_orphans(&self) -> Result<u64> {
            self.calls.lock().unwrap().push("prune".into());
            Ok(1)
        }
        async fn health(&self, _m: &str) -> Result<IndexHealth> {
            self.calls.lock().unwrap().push("health".into());
            Ok(IndexHealth {
                matching: 5,
                mismatched: 3,
                missing: 1,
            })
        }
    }

    struct StubEmbedder;
    #[async_trait]
    impl EmbeddingProvider for StubEmbedder {
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0])
        }
        fn dimensions(&self) -> usize {
            1
        }
        fn model_id(&self) -> String {
            "stub".into()
        }
    }

    use crate::user_data::mocks::mock_session::InMemorySessionStorage;

    /// Adoption must come BEFORE the prune, or the prune deletes rows adoption
    /// is about to re-create and every pass does the same work twice.
    #[tokio::test]
    async fn a_pass_adopts_before_it_prunes_and_reports_health_last() {
        let spy = Arc::new(SpyIndex::default());
        let index: Arc<dyn VectorIndex> = spy.clone();
        let report = run_index_maintenance(
            &index,
            &InMemorySessionStorage::new(),
            &StubEmbedder,
            &CancellationToken::new(),
        )
        .await;

        let calls = spy.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec!["adopt:memory", "adopt:context", "prune", "health"],
            "maintenance ran its steps in the wrong order"
        );
        assert_eq!(report.adopted, 4);
        assert_eq!(report.orphans_pruned, 1);
        assert_eq!(
            report.mismatched, 3,
            "a model change must be reported, not hidden"
        );
        assert_eq!(report.still_missing, 1);
    }

    /// A member's turn must be able to take the CPU back mid-pass.
    #[tokio::test]
    async fn a_cancelled_pass_does_no_work() {
        let spy = Arc::new(SpyIndex::default());
        let index: Arc<dyn VectorIndex> = spy.clone();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let report = run_index_maintenance(
            &index,
            &InMemorySessionStorage::new(),
            &StubEmbedder,
            &cancel,
        )
        .await;
        assert_eq!(report, MaintenanceReport::default());
    }
}
