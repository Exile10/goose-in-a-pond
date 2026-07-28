use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::peer_id::PeerId;

#[derive(Error, Debug)]
pub enum MeshTransportError {
    #[error("Peer unreachable: {0}")]
    PeerUnreachable(PeerId),

    #[error("Malformed address: {0}")]
    MalformedAddress(String),

    #[error("Transport error: {0}")]
    Transport(String),
}

/// Driven Port: MeshTransport
///
/// Low-level connectivity to other Ponds in the mesh. `address` is opaque to
/// `pond-core` — a real transport (e.g. libp2p) parses it as its own address
/// type; this port only knows it as a caller-supplied dial hint, so no
/// transport-specific type leaks into the pure domain layer.
#[async_trait]
pub trait MeshTransport: Send + Sync {
    async fn connect(&self, peer: PeerId, address: String) -> Result<(), MeshTransportError>;

    async fn send(&self, peer: PeerId, frame: Vec<u8>) -> Result<(), MeshTransportError>;

    async fn connected_peers(&self) -> Result<Vec<PeerId>, MeshTransportError>;

    /// Pull the next inbound frame from any connected peer, blocking until
    /// one arrives. Pull-based so a mock can back it with a simple queue and
    /// a real transport can back it with an mpsc fed by its event loop.
    async fn recv(&self) -> Result<(PeerId, Vec<u8>), MeshTransportError>;
}
