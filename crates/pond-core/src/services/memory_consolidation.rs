//! Memory consolidation service.
//!
//! Runs the `MemoryConsolidator` port on a batch of active memories,
//! then applies the proposed actions (merge/prune) to the repository.

use crate::domain::memory::{MemoryFragment, MemoryLifecycle};
use crate::ports::memory_consolidator::{ConsolidationAction, MemoryConsolidator};
use crate::ports::memory_repository::MemoryRepository;
use anyhow::Result;

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
        // Too few memories to consolidate meaningfully
        return Ok((0, 0));
    }

    let batch_owned: Vec<MemoryFragment> = batch.into_iter().cloned().collect();
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
                let new_frag = MemoryFragment::from_extraction(
                    uuid::Uuid::new_v4().to_string(),
                    None,
                    merged_content,
                    segment,
                    importance,
                    None,
                );
                let new_id = new_frag.id.clone();
                repo.add(new_frag).await?;

                for src_id in &source_ids {
                    let _ = repo.mark_superseded(src_id, &new_id).await;
                }
                merged += 1;
            }
            ConsolidationAction::Prune { id } => {
                let _ = repo.update_lifecycle(&id, MemoryLifecycle::Archived).await;
                pruned += 1;
            }
            ConsolidationAction::Recategorize {
                id,
                new_segment,
                new_importance,
            } => {
                let _ = repo.update_segment(&id, new_segment, new_importance).await;
                merged += 1; // count as a modification
            }
            ConsolidationAction::Split {
                source_id,
                new_memories,
            } => {
                let mut first_new_id = String::new();
                for entry in &new_memories {
                    let new_frag = MemoryFragment::from_extraction(
                        uuid::Uuid::new_v4().to_string(),
                        None,
                        entry.content.clone(),
                        entry.segment.clone(),
                        entry.importance,
                        None,
                    );
                    if first_new_id.is_empty() {
                        first_new_id = new_frag.id.clone();
                    }
                    repo.add(new_frag).await?;
                }
                if !first_new_id.is_empty() {
                    let _ = repo.mark_superseded(&source_id, &first_new_id).await;
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
