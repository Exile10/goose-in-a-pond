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

    /// This Pond's own identity. Pure lookup, no I/O — sync.
    fn local_peer_id(&self) -> PeerId;

    /// Every address this node is confirmed listening on — opaque dial
    /// hints in the same `String` shape `connect()` takes, so a caller (e.g.
    /// an invite-generation route) can hand one back to another Pond without
    /// pond-core ever knowing the transport-specific address type.
    async fn listen_addresses(&self) -> Result<Vec<String>, MeshTransportError>;
}
