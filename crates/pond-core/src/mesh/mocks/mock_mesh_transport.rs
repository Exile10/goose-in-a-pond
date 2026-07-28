use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::ports::mesh_transport::{MeshTransport, MeshTransportError};

type SentFrame = (PeerId, Vec<u8>);

/// In-memory mesh transport for testing. `connect` always succeeds unless the
/// peer has been pre-marked unreachable via [`MockMeshTransport::mark_unreachable`].
/// `recv()` is backed by a queue a test fills via [`MockMeshTransport::push_inbound`]
/// — nothing generates inbound frames on its own.
pub struct MockMeshTransport {
    connected: Arc<RwLock<HashSet<PeerId>>>,
    unreachable: Arc<RwLock<HashSet<PeerId>>>,
    sent: Arc<RwLock<Vec<SentFrame>>>,
    connect_addresses: Arc<RwLock<Vec<(PeerId, String)>>>,
    inbound_tx: mpsc::UnboundedSender<SentFrame>,
    inbound_rx: Mutex<mpsc::UnboundedReceiver<SentFrame>>,
}

impl MockMeshTransport {
    pub fn new() -> Self {
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        Self {
            connected: Arc::new(RwLock::new(HashSet::new())),
            unreachable: Arc::new(RwLock::new(HashSet::new())),
            sent: Arc::new(RwLock::new(Vec::new())),
            connect_addresses: Arc::new(RwLock::new(Vec::new())),
            inbound_tx,
            inbound_rx: Mutex::new(inbound_rx),
        }
    }

    pub async fn mark_unreachable(&self, peer: PeerId) {
        self.unreachable.write().await.insert(peer);
    }

    pub async fn sent_frames(&self) -> Vec<SentFrame> {
        self.sent.read().await.clone()
    }

    /// The `(peer, address)` pairs passed to every `connect()` call, in order.
    pub async fn connect_addresses(&self) -> Vec<(PeerId, String)> {
        self.connect_addresses.read().await.clone()
    }

    /// Queue a frame for a future `recv()` to return.
    pub fn push_inbound(&self, peer: PeerId, frame: Vec<u8>) {
        // An unbounded channel only fails to send if the receiver was
        // dropped, which can't happen while `self` is still alive.
        let _ = self.inbound_tx.send((peer, frame));
    }
}

impl Default for MockMeshTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MeshTransport for MockMeshTransport {
    async fn connect(&self, peer: PeerId, address: String) -> Result<(), MeshTransportError> {
        if self.unreachable.read().await.contains(&peer) {
            return Err(MeshTransportError::PeerUnreachable(peer));
        }
        self.connect_addresses.write().await.push((peer, address));
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

    async fn recv(&self) -> Result<(PeerId, Vec<u8>), MeshTransportError> {
        self.inbound_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| MeshTransportError::Transport("inbound channel closed".to_string()))
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
        transport
            .connect(peer, "127.0.0.1:4001".to_string())
            .await
            .unwrap();
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
        let result = transport.connect(peer, "127.0.0.1:4001".to_string()).await;
        assert!(matches!(
            result,
            Err(MeshTransportError::PeerUnreachable(_))
        ));
    }

    #[tokio::test]
    async fn connected_peers_reflects_connections() {
        let transport = MockMeshTransport::new();
        let peer = PeerId::from([4u8; 32]);
        transport
            .connect(peer, "127.0.0.1:4001".to_string())
            .await
            .unwrap();
        assert_eq!(transport.connected_peers().await.unwrap(), vec![peer]);
    }

    #[tokio::test]
    async fn connect_records_the_dial_address() {
        let transport = MockMeshTransport::new();
        let peer = PeerId::from([5u8; 32]);
        transport
            .connect(peer, "127.0.0.1:4001".to_string())
            .await
            .unwrap();
        assert_eq!(
            transport.connect_addresses().await,
            vec![(peer, "127.0.0.1:4001".to_string())]
        );
    }

    #[tokio::test]
    async fn recv_returns_pushed_frame() {
        let transport = MockMeshTransport::new();
        let peer = PeerId::from([6u8; 32]);
        transport.push_inbound(peer, vec![9, 9, 9]);
        let (from, frame) = transport.recv().await.unwrap();
        assert_eq!(from, peer);
        assert_eq!(frame, vec![9, 9, 9]);
    }

    #[tokio::test]
    async fn recv_returns_frames_in_order() {
        let transport = MockMeshTransport::new();
        let peer = PeerId::from([7u8; 32]);
        transport.push_inbound(peer, vec![1]);
        transport.push_inbound(peer, vec![2]);
        assert_eq!(transport.recv().await.unwrap().1, vec![1]);
        assert_eq!(transport.recv().await.unwrap().1, vec![2]);
    }
}
