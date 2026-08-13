//! Indexing conversation summaries into the personal-context index (phase B).
//!
//! Memories and context items reach the index for free: both already hold a
//! vector by the time they are stored, so their write-through just mirrors what
//! is in hand. **Summaries do not.** Nothing in this pond has ever embedded
//! `sessions.rolling_summary`, so this is genuinely new inference and is the
//! only part of phase B with a cost.
//!
//! That cost decides the shape. This is a SWEEP, not a write-through:
//!
//! * The two writers of that column, [`SessionSummaryService::refresh`] and
//!   `::resummarise`, are already expensive — each is an LLM call — and one of
//!   them runs on the compaction path. Embedding inside them would stack a
//!   second model's work onto a path that is already the slow one.
//! * A summary is rewritten IN PLACE, so a write-through would have to be an
//!   upsert anyway; the index already keys on `(corpus, row_id)` and
//!   `needs_embedding` already compares `source_rev` against
//!   `rolling_summary_updated_at`. The sweep therefore detects a re-summarised
//!   session with no help from the writer, which is the property phase B is
//!   asked to demonstrate: **a re-summarised session's vector changes.**
//! * A sweep is restartable and self-healing. A write-through that fails leaves
//!   a summary permanently unsearchable with nothing to notice.
//!
//! [`SessionSummaryService::refresh`]: crate::shared::services::session_summary::SessionSummaryService::refresh

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::context::vector_index::{Corpus, VectorEntry, VectorIndex};
use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::ports::session_storage::SessionStorage;

/// Summaries embedded per batch, then a pause. Matches the memory backfill: on a
/// Jetson this competes with inference for CPU, so it yields rather than running
/// flat out.
pub const SUMMARY_BATCH_SIZE: usize = 16;

/// Pause between batches, in milliseconds.
pub const SUMMARY_BATCH_PAUSE_MS: u64 = 250;

/// Embed and index every session summary that has no current vector.
///
/// "No current vector" is whatever [`VectorIndex::needs_embedding`] says, which
/// covers three cases with one query: never embedded, embedded by a different
/// model, and embedded before the summary was rewritten.
///
/// Cancellable, because it is driven from the same idle path as the summary
/// refresh itself and a user turn must be able to take the CPU back. Returns how
/// many summaries were indexed.
pub async fn run_summary_indexing(
    storage: &dyn SessionStorage,
    embedder: &dyn EmbeddingProvider,
    index: &Arc<dyn VectorIndex>,
    cancel: &CancellationToken,
    batch_size: usize,
    pause_ms: u64,
) -> usize {
    let model_id = embedder.model_id();
    let mut indexed = 0usize;

    loop {
        if cancel.is_cancelled() {
            break;
        }
        let batch = match index
            .needs_embedding(Corpus::Summary, &model_id, batch_size)
            .await
        {
            Ok(rows) if rows.is_empty() => break,
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("[summary-index] fetch failed: {e}");
                break;
            }
        };

        let mut progressed = false;
        for session_id in &batch {
            if cancel.is_cancelled() {
                break;
            }
            // Read the summary text and its revision together, so the vector is
            // stamped with the revision it was actually computed from. Reading
            // them separately would race the summary service and stamp a vector
            // with a newer revision than the text it embedded — which would make
            // a stale vector look current and never be repaired.
            let (summary, rev) = match storage.get_rolling_summary_with_revision(session_id).await {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(session_id = %session_id, "[summary-index] read failed: {e}");
                    continue;
                }
            };
            let Some(text) = summary.filter(|s| !s.trim().is_empty()) else {
                // The row qualified when the query ran and does not now. Nothing
                // to embed; the next sweep will agree.
                continue;
            };
            match embedder.embed(&text).await {
                Ok(vector) => {
                    let entry = VectorEntry {
                        corpus: Corpus::Summary,
                        row_id: session_id.clone(),
                        model_id: model_id.clone(),
                        vector,
                        source_rev: rev,
                    };
                    match index.upsert(&entry).await {
                        Ok(()) => {
                            indexed += 1;
                            progressed = true;
                        }
                        Err(e) => {
                            tracing::warn!(session_id = %session_id, "[summary-index] store failed: {e}")
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(session_id = %session_id, "[summary-index] embed failed: {e}")
                }
            }
        }

        // Every row in the batch failed, so the same rows would come back
        // forever — stop rather than spin.
        if !progressed {
            tracing::warn!("[summary-index] no progress in a batch — stopping");
            break;
        }
        if pause_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(pause_ms)).await;
        }
    }

    if indexed > 0 {
        tracing::info!("[summary-index] indexed {indexed} conversation summaries");
    }
    indexed
}
