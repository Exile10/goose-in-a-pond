//! Indexing conversation summaries into the personal-context index (phase B). Memories and context
//! items already hold a vector when stored; `sessions.rolling_summary` never has, so this is a
//! restartable sweep rather than a write-through: its writers are already LLM calls, and the index
//! re-detects a re-summarised session by comparing `source_rev` to `rolling_summary_updated_at`.

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

/// Embed and index every session summary that has no current vector, meaning whatever
/// [`VectorIndex::needs_embedding`] says: never embedded, embedded by a different model, or
/// embedded before the summary was rewritten. Cancellable because it runs on the idle path and a
/// user turn must be able to take the CPU back. Returns how many summaries were indexed.
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
            // Read the summary text and its revision together so the vector is stamped with the
            // revision it was computed from. Reading them separately races the summary service
            // and stamps a newer revision than the embedded text, so a stale vector looks current.
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
                    // A rolling summary is already a compression of a whole
                    // conversation; chunking a compression would be splitting
                    // the summary of a thing rather than the thing.
                    let entry = VectorEntry::whole(
                        Corpus::Summary,
                        session_id.clone(),
                        model_id.clone(),
                        vector,
                        rev,
                    );
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
