//! Memory consolidation service.
//!
//! Runs the `MemoryConsolidator` port on a batch of active memories,
//! then applies the proposed actions (merge/prune) to the repository.
//!
//! Safety guards:
//! - Correction memories are never pruned
//! - Merges involving correction sources preserve the `corrects` metadata
//!   and force the merged segment to `Correction`
//! - Every lifecycle change is logged as a `MemoryEvent` for auditability

use crate::domain::memory::{MemoryEventKind, MemoryFragment, MemoryLifecycle, MemorySegment};
use crate::ports::memory_consolidator::{ConsolidationAction, MemoryConsolidator};
use crate::ports::memory_repository::MemoryRepository;
use anyhow::Result;
use std::collections::HashMap;

/// Run one consolidation pass: fetch active memories, propose actions, apply them.
///
/// `batch_size` — max memories to process (default 50, capped by model context).
///
/// Returns (merged, pruned) counts.
pub async fn run_consolidation(
    consolidator: &dyn MemoryConsolidator,
    repo: &dyn MemoryRepository,
    batch_size: usize,
) -> Result<(usize, usize)> {
    let memories = repo.search_scoreable(None).await?;
    let batch: Vec<&MemoryFragment> = memories
        .iter()
        .filter(|m| m.segment.is_some())
        .take(batch_size)
        .collect();

    if batch.len() < 6 {
        return Ok((0, 0));
    }

    let batch_owned: Vec<MemoryFragment> = batch.into_iter().cloned().collect();

    // Build lookup for source memory metadata (corrects, segment)
    let memory_map: HashMap<&str, &MemoryFragment> =
        batch_owned.iter().map(|m| (m.id.as_str(), m)).collect();

    let actions = consolidator.consolidate(&batch_owned).await?;

    let mut merged = 0;
    let mut pruned = 0;
    let mut split = 0;

    for action in actions {
        match action {
            ConsolidationAction::Merge {
                source_ids,
                merged_content,
                segment,
                importance,
            } => {
                // Guard: if any source is a correction, preserve its metadata
                let any_correction = source_ids
                    .iter()
                    .filter_map(|id| memory_map.get(id.as_str()))
                    .any(|m| m.is_correction());

                let effective_segment = if any_correction {
                    MemorySegment::Correction
                } else {
                    segment
                };

                let corrects = source_ids
                    .iter()
                    .filter_map(|id| memory_map.get(id.as_str()))
                    .filter_map(|m| m.corrects.clone())
                    .next();

                if any_correction {
                    tracing::info!(
                        "[consolidation] merge includes correction source — forcing segment=Correction"
                    );
                }

                let new_frag = MemoryFragment::from_extraction(
                    uuid::Uuid::new_v4().to_string(),
                    None,
                    merged_content,
                    effective_segment,
                    importance,
                    corrects,
                );
                let new_id = new_frag.id.clone();
                repo.add(new_frag).await?;

                for src_id in &source_ids {
                    let _ = repo.mark_superseded(src_id, &new_id).await;
                    let _ = repo
                        .log_event(MemoryEventKind::Superseded, src_id, None, Some(&new_id))
                        .await;
                }
                let _ = repo
                    .log_event(MemoryEventKind::Consolidated, &new_id, None, None)
                    .await;
                merged += 1;
            }
            ConsolidationAction::Prune { id } => {
                // Guard: never prune correction memories
                if let Some(mem) = memory_map.get(id.as_str()) {
                    if mem.is_correction() {
                        tracing::warn!("[consolidation] blocked prune of correction memory {id}");
                        continue;
                    }
                }

                let _ = repo.update_lifecycle(&id, MemoryLifecycle::Archived).await;
                let _ = repo
                    .log_event(MemoryEventKind::Pruned, &id, None, None)
                    .await;
                pruned += 1;
            }
            ConsolidationAction::Recategorize {
                id,
                new_segment,
                new_importance,
            } => {
                let _ = repo.update_segment(&id, new_segment, new_importance).await;
                let _ = repo
                    .log_event(
                        MemoryEventKind::Consolidated,
                        &id,
                        None,
                        Some("recategorized"),
                    )
                    .await;
                merged += 1;
            }
            ConsolidationAction::Split {
                source_id,
                new_memories,
            } => {
                // If source is a correction, propagate corrects to the first correction-segment entry
                let source_corrects = memory_map
                    .get(source_id.as_str())
                    .and_then(|m| m.corrects.clone());

                let mut first_new_id = String::new();
                let mut corrects_assigned = false;

                for entry in &new_memories {
                    // Assign corrects to the first Correction-segment split entry
                    let entry_corrects = if !corrects_assigned
                        && source_corrects.is_some()
                        && entry.segment == MemorySegment::Correction
                    {
                        corrects_assigned = true;
                        source_corrects.clone()
                    } else {
                        None
                    };

                    let new_frag = MemoryFragment::from_extraction(
                        uuid::Uuid::new_v4().to_string(),
                        None,
                        entry.content.clone(),
                        entry.segment.clone(),
                        entry.importance,
                        entry_corrects,
                    );
                    let nid = new_frag.id.clone();
                    if first_new_id.is_empty() {
                        first_new_id = nid.clone();
                    }
                    repo.add(new_frag).await?;
                    let _ = repo
                        .log_event(MemoryEventKind::Consolidated, &nid, None, None)
                        .await;
                }
                if !first_new_id.is_empty() {
                    let _ = repo.mark_superseded(&source_id, &first_new_id).await;
                    let _ = repo
                        .log_event(
                            MemoryEventKind::Superseded,
                            &source_id,
                            None,
                            Some(&first_new_id),
                        )
                        .await;
                }
                split += 1;
            }
        }
    }

    if merged > 0 || pruned > 0 || split > 0 {
        tracing::info!("[memory-consolidation] merged={merged}, pruned={pruned}, split={split}");
    }

    Ok((merged, pruned))
}
