use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::peer_id::PeerId;

#[derive(Error, Debug)]
pub enum MeshTransportError {
    #[error("Peer unreachable: {0}")]
    PeerUnreachable(PeerId),

    #[error("Transport error: {0}")]
    Transport(String),
}

/// Driven Port: MeshTransport
///
/// Low-level connectivity to other Ponds in the mesh. The wire format and
/// receive-side event stream are defined once a concrete transport (the
/// planned libp2p adapter) exists — this port only covers what every caller
/// needs regardless of transport: connect, send a frame, and see who's up.
#[async_trait]
pub trait MeshTransport: Send + Sync {
    async fn connect(&self, peer: PeerId) -> Result<(), MeshTransportError>;

    async fn send(&self, peer: PeerId, frame: Vec<u8>) -> Result<(), MeshTransportError>;

    async fn connected_peers(&self) -> Result<Vec<PeerId>, MeshTransportError>;
}
