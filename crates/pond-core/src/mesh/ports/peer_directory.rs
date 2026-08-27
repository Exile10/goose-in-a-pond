use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::domain::trust_scope::TrustScope;

#[derive(Error, Debug)]
pub enum PeerDirectoryError {
    #[error("Peer not found: {0}")]
    PeerNotFound(PeerId),

    #[error("Directory error: {0}")]
    General(String),
}

/// Driven Port: PeerDirectory
///
/// Owns which peers this Pond trusts and at what scope. `MeshTransport`
/// consults this before connecting; a peer absent from the directory is not
/// trusted at all.
#[async_trait]
pub trait PeerDirectory: Send + Sync {
    async fn add_trusted_peer(
        &self,
        peer: PeerId,
        scope: TrustScope,
    ) -> Result<(), PeerDirectoryError>;

    async fn remove_trusted_peer(&self, peer: PeerId) -> Result<(), PeerDirectoryError>;

    async fn list_trusted_peers(
        &self,
        scope: Option<TrustScope>,
    ) -> Result<Vec<PeerId>, PeerDirectoryError>;

    async fn trust_scope_of(&self, peer: PeerId) -> Result<Option<TrustScope>, PeerDirectoryError>;

    /// Remember the last address a peer was successfully dialed at, so a
    /// restart doesn't lose the ability to auto-reconnect to it. Best-effort
    /// bookkeeping, not part of the trust model: a peer with no recorded
    /// address is simply absent from `known_addresses`.
    async fn record_peer_address(
        &self,
        peer: PeerId,
        address: String,
    ) -> Result<(), PeerDirectoryError>;

    /// Every trusted peer for which an address has been recorded, for
    /// seeding the mesh transport's reconnect loop at startup.
    async fn known_addresses(&self) -> Result<Vec<(PeerId, String)>, PeerDirectoryError>;
}
