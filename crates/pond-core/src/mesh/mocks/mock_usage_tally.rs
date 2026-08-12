use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::domain::token_count::TokenCount;
use crate::mesh::ports::usage_tally::{UsageTally, UsageTallyError};

/// In-memory usage tally for testing. A peer with no recorded usage has a
/// zero pending tally rather than a missing entry.
pub struct MockUsageTally {
    pending: Arc<RwLock<HashMap<PeerId, TokenCount>>>,
}

impl MockUsageTally {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MockUsageTally {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl UsageTally for MockUsageTally {
    async fn record_usage(&self, peer: PeerId, tokens: TokenCount) -> Result<(), UsageTallyError> {
        let mut pending = self.pending.write().await;
        let current = pending.get(&peer).copied().unwrap_or(TokenCount::new(0));
        let updated = current
            .checked_add(tokens)
            .ok_or_else(|| UsageTallyError::General("tally overflow".to_string()))?;
        pending.insert(peer, updated);
        Ok(())
    }

    async fn pending_tally(&self, peer: PeerId) -> Result<TokenCount, UsageTallyError> {
        Ok(self
            .pending
            .read()
            .await
            .get(&peer)
            .copied()
            .unwrap_or(TokenCount::new(0)))
    }

    async fn mark_settled(&self, peer: PeerId, up_to: TokenCount) -> Result<(), UsageTallyError> {
        let mut pending = self.pending.write().await;
        let current = pending.get(&peer).copied().unwrap_or(TokenCount::new(0));
        let remaining = current
            .checked_sub(up_to)
            .ok_or_else(|| UsageTallyError::General("settled more than pending".to_string()))?;
        pending.insert(peer, remaining);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn trait_object_conformance() {
        let _: StdArc<dyn UsageTally> = StdArc::new(MockUsageTally::new());
    }

    #[tokio::test]
    async fn record_usage_accumulates() {
        let tally = MockUsageTally::new();
        let peer = PeerId::from([1u8; 32]);
        tally
            .record_usage(peer, TokenCount::new(100))
            .await
            .unwrap();
        tally.record_usage(peer, TokenCount::new(50)).await.unwrap();
        assert_eq!(
            tally.pending_tally(peer).await.unwrap(),
            TokenCount::new(150)
        );
    }

    #[tokio::test]
    async fn mark_settled_reduces_pending() {
        let tally = MockUsageTally::new();
        let peer = PeerId::from([2u8; 32]);
        tally
            .record_usage(peer, TokenCount::new(100))
            .await
            .unwrap();
        tally.mark_settled(peer, TokenCount::new(60)).await.unwrap();
        assert_eq!(
            tally.pending_tally(peer).await.unwrap(),
            TokenCount::new(40)
        );
    }

    #[tokio::test]
    async fn mark_settled_more_than_pending_errors() {
        let tally = MockUsageTally::new();
        let peer = PeerId::from([3u8; 32]);
        tally.record_usage(peer, TokenCount::new(10)).await.unwrap();
        let result = tally.mark_settled(peer, TokenCount::new(11)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unknown_peer_has_zero_pending() {
        let tally = MockUsageTally::new();
        let peer = PeerId::from([4u8; 32]);
        assert_eq!(tally.pending_tally(peer).await.unwrap(), TokenCount::new(0));
    }
}
