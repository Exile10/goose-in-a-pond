use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::domain::trust_scope::TrustScope;
use crate::mesh::ports::peer_directory::{PeerDirectory, PeerDirectoryError};

/// In-memory peer directory for testing.
pub struct MockPeerDirectory {
    peers: Arc<RwLock<HashMap<PeerId, TrustScope>>>,
    addresses: Arc<RwLock<HashMap<PeerId, String>>>,
}

impl MockPeerDirectory {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
            addresses: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MockPeerDirectory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PeerDirectory for MockPeerDirectory {
    async fn add_trusted_peer(
        &self,
        peer: PeerId,
        scope: TrustScope,
    ) -> Result<(), PeerDirectoryError> {
        self.peers.write().await.insert(peer, scope);
        Ok(())
    }

    async fn remove_trusted_peer(&self, peer: PeerId) -> Result<(), PeerDirectoryError> {
        self.peers.write().await.remove(&peer);
        Ok(())
    }

    async fn list_trusted_peers(
        &self,
        scope: Option<TrustScope>,
    ) -> Result<Vec<PeerId>, PeerDirectoryError> {
        let peers = self.peers.read().await;
        Ok(peers
            .iter()
            .filter(|(_, s)| scope.is_none_or(|want| **s == want))
            .map(|(id, _)| *id)
            .collect())
    }

    async fn trust_scope_of(&self, peer: PeerId) -> Result<Option<TrustScope>, PeerDirectoryError> {
        Ok(self.peers.read().await.get(&peer).copied())
    }

    async fn record_peer_address(
        &self,
        peer: PeerId,
        address: String,
    ) -> Result<(), PeerDirectoryError> {
        self.addresses.write().await.insert(peer, address);
        Ok(())
    }

    async fn known_addresses(&self) -> Result<Vec<(PeerId, String)>, PeerDirectoryError> {
        Ok(self
            .addresses
            .read()
            .await
            .iter()
            .map(|(peer, addr)| (*peer, addr.clone()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn trait_object_conformance() {
        let _: StdArc<dyn PeerDirectory> = StdArc::new(MockPeerDirectory::new());
    }

    #[tokio::test]
    async fn add_then_lookup_scope() {
        let dir = MockPeerDirectory::new();
        let peer = PeerId::from([1u8; 32]);
        dir.add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
        assert_eq!(
            dir.trust_scope_of(peer).await.unwrap(),
            Some(TrustScope::Circle)
        );
    }

    #[tokio::test]
    async fn unknown_peer_has_no_scope() {
        let dir = MockPeerDirectory::new();
        let peer = PeerId::from([2u8; 32]);
        assert_eq!(dir.trust_scope_of(peer).await.unwrap(), None);
    }

    #[tokio::test]
    async fn remove_clears_trust() {
        let dir = MockPeerDirectory::new();
        let peer = PeerId::from([3u8; 32]);
        dir.add_trusted_peer(peer, TrustScope::SelfOwned)
            .await
            .unwrap();
        dir.remove_trusted_peer(peer).await.unwrap();
        assert_eq!(dir.trust_scope_of(peer).await.unwrap(), None);
    }

    #[tokio::test]
    async fn list_trusted_peers_filters_by_scope() {
        let dir = MockPeerDirectory::new();
        let self_owned = PeerId::from([4u8; 32]);
        let circle = PeerId::from([5u8; 32]);
        dir.add_trusted_peer(self_owned, TrustScope::SelfOwned)
            .await
            .unwrap();
        dir.add_trusted_peer(circle, TrustScope::Circle)
            .await
            .unwrap();

        let circle_only = dir
            .list_trusted_peers(Some(TrustScope::Circle))
            .await
            .unwrap();
        assert_eq!(circle_only, vec![circle]);

        let all = dir.list_trusted_peers(None).await.unwrap();
        assert_eq!(all.len(), 2);
    }
}
