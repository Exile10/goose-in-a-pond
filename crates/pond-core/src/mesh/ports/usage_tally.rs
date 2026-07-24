use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::domain::token_count::TokenCount;

#[derive(Error, Debug)]
pub enum UsageTallyError {
    #[error("Tally error: {0}")]
    General(String),
}

/// Driven Port: UsageTally
///
/// Accumulates metered token usage with a peer between settlements.
/// `record_usage` is the hot-path call (once per served request);
/// `mark_settled` is called by the same background job that calls
/// `PaymentRail::batch_settle`, never inline with a request.
#[async_trait]
pub trait UsageTally: Send + Sync {
    async fn record_usage(&self, peer: PeerId, tokens: TokenCount) -> Result<(), UsageTallyError>;

    async fn pending_tally(&self, peer: PeerId) -> Result<TokenCount, UsageTallyError>;

    async fn mark_settled(&self, peer: PeerId, up_to: TokenCount) -> Result<(), UsageTallyError>;
}
