//! AdversarialConsolidator port — two-agent memory consolidation.
//!
//! A Prosecutor agent proposes prune/merge actions, then a Defender agent
//! challenges each proposal. Only proposals that the Defender agrees with
//! are accepted. This adversarial protocol is more conservative than single-
//! pass consolidation, reducing the risk of losing valuable memories.

use crate::domain::memory::MemoryFragment;
use crate::ports::memory_consolidator::{AdversarialConsolidationResult, ConsolidationEvent};
use anyhow::Result;
use async_trait::async_trait;

/// Driven port: adversarial memory consolidation with prosecutor + defender agents.
#[async_trait]
pub trait AdversarialConsolidator: Send + Sync {
    /// Run the full adversarial protocol on a batch of memories.
    ///
    /// If `event_tx` is provided, progress events are emitted for each
    /// proposal and verdict (useful for SSE streaming to the UI).
    async fn consolidate(
        &self,
        memories: &[MemoryFragment],
        event_tx: Option<tokio::sync::mpsc::Sender<ConsolidationEvent>>,
    ) -> Result<AdversarialConsolidationResult>;
}
