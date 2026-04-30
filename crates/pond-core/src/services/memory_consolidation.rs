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
/// Returns (merged, pruned) counts.
pub async fn run_consolidation(
    consolidator: &dyn MemoryConsolidator,
    repo: &dyn MemoryRepository,
) -> Result<(usize, usize)> {
    // Fetch active memories with segment data (max 50 to stay within model context)
    let memories = repo.search_scoreable(None).await?;
    let batch: Vec<&MemoryFragment> = memories
        .iter()
        .filter(|m| m.segment.is_some())
        .take(50)
        .collect();

    if batch.len() < 3 {
        // Too few memories to consolidate
        return Ok((0, 0));
    }

    let batch_owned: Vec<MemoryFragment> = batch.into_iter().cloned().collect();
    let actions = consolidator.consolidate(&batch_owned).await?;

    let mut merged = 0;
    let mut pruned = 0;

    for action in actions {
        match action {
            ConsolidationAction::Merge {
                source_ids,
                merged_content,
                segment,
                importance,
            } => {
                // Create the merged memory
                let new_frag = MemoryFragment::from_extraction(
                    uuid::Uuid::new_v4().to_string(),
                    None,
                    merged_content,
                    segment,
                    importance,
                );
                let new_id = new_frag.id.clone();
                repo.add(new_frag).await?;

                // Mark source memories as merged
                for src_id in &source_ids {
                    let _ = repo.mark_superseded(src_id, &new_id).await;
                }
                merged += 1;
            }
            ConsolidationAction::Prune { id } => {
                let _ = repo.update_lifecycle(&id, MemoryLifecycle::Archived).await;
                pruned += 1;
            }
        }
    }

    if merged > 0 || pruned > 0 {
        tracing::info!("[memory-consolidation] merged={merged}, pruned={pruned}");
    }

    Ok((merged, pruned))
}
