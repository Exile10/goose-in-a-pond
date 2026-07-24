use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::ports::mesh_transport::{MeshTransport, MeshTransportError};

type SentFrame = (PeerId, Vec<u8>);

/// In-memory mesh transport for testing. `connect` always succeeds unless the
/// peer has been pre-marked unreachable via [`MockMeshTransport::mark_unreachable`].
pub struct MockMeshTransport {
    connected: Arc<RwLock<HashSet<PeerId>>>,
    unreachable: Arc<RwLock<HashSet<PeerId>>>,
    sent: Arc<RwLock<Vec<SentFrame>>>,
}

impl MockMeshTransport {
    pub fn new() -> Self {
        Self {
            connected: Arc::new(RwLock::new(HashSet::new())),
            unreachable: Arc::new(RwLock::new(HashSet::new())),
            sent: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn mark_unreachable(&self, peer: PeerId) {
        self.unreachable.write().await.insert(peer);
    }

    pub async fn sent_frames(&self) -> Vec<SentFrame> {
        self.sent.read().await.clone()
    }
}

impl Default for MockMeshTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MeshTransport for MockMeshTransport {
    async fn connect(&self, peer: PeerId) -> Result<(), MeshTransportError> {
        if self.unreachable.read().await.contains(&peer) {
            return Err(MeshTransportError::PeerUnreachable(peer));
        }
        self.connected.write().await.insert(peer);
        Ok(())
    }

    async fn send(&self, peer: PeerId, frame: Vec<u8>) -> Result<(), MeshTransportError> {
        if !self.connected.read().await.contains(&peer) {
            return Err(MeshTransportError::PeerUnreachable(peer));
        }
        self.sent.write().await.push((peer, frame));
        Ok(())
    }

    async fn connected_peers(&self) -> Result<Vec<PeerId>, MeshTransportError> {
        Ok(self.connected.read().await.iter().copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn trait_object_conformance() {
        let _: StdArc<dyn MeshTransport> = StdArc::new(MockMeshTransport::new());
    }

    #[tokio::test]
    async fn connect_then_send_succeeds() {
        let transport = MockMeshTransport::new();
        let peer = PeerId::from([1u8; 32]);
        transport.connect(peer).await.unwrap();
        transport.send(peer, vec![1, 2, 3]).await.unwrap();
        assert_eq!(transport.sent_frames().await, vec![(peer, vec![1, 2, 3])]);
    }

    #[tokio::test]
    async fn send_without_connect_fails() {
        let transport = MockMeshTransport::new();
        let peer = PeerId::from([2u8; 32]);
        let result = transport.send(peer, vec![1]).await;
        assert!(matches!(
            result,
            Err(MeshTransportError::PeerUnreachable(_))
        ));
    }

    #[tokio::test]
    async fn connect_to_marked_unreachable_peer_fails() {
        let transport = MockMeshTransport::new();
        let peer = PeerId::from([3u8; 32]);
        transport.mark_unreachable(peer).await;
        let result = transport.connect(peer).await;
        assert!(matches!(
            result,
            Err(MeshTransportError::PeerUnreachable(_))
        ));
    }

    #[tokio::test]
    async fn connected_peers_reflects_connections() {
        let transport = MockMeshTransport::new();
        let peer = PeerId::from([4u8; 32]);
        transport.connect(peer).await.unwrap();
        assert_eq!(transport.connected_peers().await.unwrap(), vec![peer]);
    }
}
