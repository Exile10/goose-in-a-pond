use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::capabilities::PeerCapabilities;
use crate::mesh::domain::peer_id::PeerId;

#[derive(Error, Debug)]
pub enum PeerCapabilityQueryError {
    #[error("mesh transport error: {0}")]
    Transport(String),

    #[error("no response from peer {0} before timeout")]
    Timeout(PeerId),
}

/// Driven Port: PeerCapabilityQuery
///
/// A live, on-demand question — "what does this peer offer right now?" — not
/// a separate port from `PeerDirectory` by accident: `PeerDirectory` is
/// SQLite-persisted trust data (survives a restart by design); a peer's
/// active capabilities can flip between two consecutive queries (Lightning
/// wallet connects/disconnects, backing model swapped out), so caching them
/// alongside trust scopes would make stale data look authoritative.
#[async_trait]
pub trait PeerCapabilityQuery: Send + Sync {
    async fn capabilities_of(
        &self,
        peer: PeerId,
    ) -> Result<PeerCapabilities, PeerCapabilityQueryError>;
}
