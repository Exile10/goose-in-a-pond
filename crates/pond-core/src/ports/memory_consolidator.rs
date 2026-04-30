//! MemoryConsolidator port — merges duplicate/contradicting memories.
//!
//! Runs periodically as a background task. The adapter uses the LLM
//! to propose merge/prune actions on the current memory set.

use crate::domain::memory::{MemoryFragment, MemorySegment};
use anyhow::Result;
use async_trait::async_trait;

/// An action proposed by the consolidation pass.
#[derive(Debug, Clone)]
pub enum ConsolidationAction {
    /// Merge multiple memories into one with new content.
    Merge {
        source_ids: Vec<String>,
        merged_content: String,
        segment: MemorySegment,
        importance: f32,
    },
    /// Remove a redundant or low-value memory.
    Prune { id: String },
}

/// Driven port: consolidate memories by merging duplicates and pruning redundancy.
#[async_trait]
pub trait MemoryConsolidator: Send + Sync {
    /// Analyse a batch of memories and propose consolidation actions.
    async fn consolidate(
        &self,
        memories: &[MemoryFragment],
    ) -> Result<Vec<ConsolidationAction>>;
}
