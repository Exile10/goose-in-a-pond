//! In-process, loopback-only integration tests for `Libp2pMeshTransport`.
//! Nothing here touches real hardware or the public network, so no test is
//! `#[ignore]`d.

use std::sync::Arc;
use std::time::Duration;

use pond_adapters_mesh_libp2p::identity::domain_peer_to_libp2p;
use pond_adapters_mesh_libp2p::{Libp2pMeshTransport, Libp2pMeshTransportConfig};
use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::domain::trust_scope::TrustScope;
use pond_core::mesh::mocks::mock_peer_directory::MockPeerDirectory;
use pond_core::mesh::ports::mesh_transport::MeshTransport;
use pond_core::mesh::ports::peer_directory::PeerDirectory;
use pond_mesh_protocol::identity::MeshKeypair;

fn harness() -> HarnessHash {
    HarnessHash::from([1u8; 32])
}

fn model() -> ModelHash {
    ModelHash::from([2u8; 32])
}

fn other_model() -> ModelHash {
    ModelHash::from([9u8; 32])
}

/// A node plus the directory that decides who it will speak to, so a test can
/// grant trust after the node exists (the peer ids are only known then).
struct Node {
    transport: Libp2pMeshTransport,
    directory: Arc<MockPeerDirectory>,
}

impl std::ops::Deref for Node {
    type Target = Libp2pMeshTransport;
    fn deref(&self) -> &Self::Target {
        &self.transport
    }
}

impl Node {
    async fn trust(&self, peer: PeerId) {
        self.directory
            .add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
    }
}

async fn spawn_node(model_hash: ModelHash) -> Node {
    let directory = Arc::new(MockPeerDirectory::new());
    let config = Libp2pMeshTransportConfig {
        listen_addr: "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
        harness_hash: harness(),
        model_hash,
        keypair: MeshKeypair::generate(),
        peer_directory: directory.clone(),
    };
    Node {
        transport: Libp2pMeshTransport::new(config).unwrap(),
        directory,
    }
}

/// Put two nodes in each other's trust circle. Trust is not symmetric in the
/// domain, so both directions are stated rather than assumed.
async fn trust_each_other(a: &Node, b: &Node) {
    a.trust(b.local_peer_id()).await;
    b.trust(a.local_peer_id()).await;
}

/// Poll `listen_addresses()` until at least one address is confirmed —
/// binding to `tcp/0` resolves the actual port asynchronously.
async fn wait_for_listen_address(node: &Node) -> String {
    for _ in 0..200 {
        let addrs = node.listen_addresses().await.unwrap();
        if let Some(addr) = addrs.into_iter().find(|a| !a.contains("p2p-circuit")) {
            return addr;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("node never reported a listen address");
}

#[tokio::test]
async fn trait_object_conformance() {
    let node = spawn_node(model()).await;
    let _: Arc<dyn MeshTransport> = Arc::new(node.transport);
}

#[tokio::test]
async fn connect_send_recv_roundtrip() {
    let a = spawn_node(model()).await;
    let b = spawn_node(model()).await;
    trust_each_other(&a, &b).await;
    let b_addr = wait_for_listen_address(&b).await;

    a.connect(b.local_peer_id(), b_addr).await.unwrap();

    assert_eq!(a.connected_peers().await.unwrap(), vec![b.local_peer_id()]);
    // The listener side only learns about a peer once its handshake request
    // arrives — give the event loop a beat to process it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if b.connected_peers().await.unwrap() == vec![a.local_peer_id()] {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "b never saw a as connected"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    a.send(b.local_peer_id(), vec![1, 2, 3]).await.unwrap();
    let (from, frame) = tokio::time::timeout(Duration::from_secs(5), b.recv())
        .await
        .expect("recv timed out")
        .unwrap();
    assert_eq!(from, a.local_peer_id());
    assert_eq!(frame, vec![1, 2, 3]);
}

#[tokio::test]
async fn mismatched_model_hash_is_refused() {
    let a = spawn_node(model()).await;
    let b = spawn_node(other_model()).await;
    // Fully trusted on both sides, so the refusal below can only be the hash
    // pin -- otherwise this test would pass for the wrong reason once the
    // trust gate landed.
    trust_each_other(&a, &b).await;
    let b_addr = wait_for_listen_address(&b).await;

    let result = a.connect(b.local_peer_id(), b_addr).await;
    assert!(
        result.is_err(),
        "connect should be refused on hash mismatch"
    );

    // Give any in-flight state a moment to settle, then confirm neither side
    // considers the other connected.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(a.connected_peers().await.unwrap().is_empty());
    assert!(b.connected_peers().await.unwrap().is_empty());
}

#[tokio::test]
async fn relay_mediated_connect() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    // A and B never learn each other's direct address — only C's (the
    // relay). B reserves a slot on C; A dials B purely via C's circuit.
    let a = spawn_node(model()).await;
    let b = spawn_node(model()).await;
    let c = spawn_node(model()).await;
    trust_each_other(&a, &b).await;
    trust_each_other(&b, &c).await;
    // A never dials C directly, but C relays for A, so C must know A.
    trust_each_other(&a, &c).await;
    let c_addr = wait_for_listen_address(&c).await;

    // A relay reservation is sent over an existing connection to the relay —
    // establish one first (this also happens to prove C treats B as an
    // ordinary verified peer, not anything relay-specific).
    b.connect(c.local_peer_id(), c_addr.clone()).await.unwrap();

    // The reservation is rejected until B has learned at least one external
    // address to advertise, which arrives asynchronously via an `identify`
    // exchange with C — so retry until it lands rather than relying on a
    // single well-timed attempt.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let _ = b.reserve_relay(c.local_peer_id(), c_addr.clone()).await;
        if b.listen_addresses()
            .await
            .unwrap()
            .iter()
            .any(|a| a.contains("p2p-circuit"))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "b's relay reservation was never accepted"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let c_libp2p_peer = domain_peer_to_libp2p(c.local_peer_id()).unwrap();
    let circuit_addr = format!("{c_addr}/p2p/{c_libp2p_peer}/p2p-circuit");

    a.connect(b.local_peer_id(), circuit_addr).await.unwrap();
    assert_eq!(a.connected_peers().await.unwrap(), vec![b.local_peer_id()]);
}

// ── What the handshake is actually for ───────────────────────────────────
//
// The three tests below are the ones that fail if the authentication work is
// reverted. They are written as behaviour at the port, not as unit tests of
// `verify_handshake`, because the defect they cover was never in that
// function's arithmetic -- it was in what the function was never asked.

/// An untrusted peer is refused, even though it is running exactly this build
/// and exactly this model.
///
/// This is the distinction the whole feature rests on. The handshake proves
/// "same harness, same model, holds the key it claims" -- all three of which
/// are true of every GIAP pond on the internet running this release. Trust is
/// what makes a mesh private, and `PeerDirectory`'s port doc has always said
/// the transport consults it: "a peer absent from the directory is not trusted
/// at all".
#[tokio::test]
async fn an_untrusted_peer_is_refused_even_on_a_matching_build() {
    let a = spawn_node(model()).await;
    let b = spawn_node(model()).await;
    // Deliberately NO trust in either direction. Same harness, same model.
    let b_addr = wait_for_listen_address(&b).await;

    let result = a.connect(b.local_peer_id(), b_addr).await;
    assert!(
        result.is_err(),
        "a stranger running the same build completed a handshake: the trust \
         circle is decorative"
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        a.connected_peers().await.unwrap().is_empty(),
        "a recorded an untrusted peer as connected"
    );
    assert!(
        b.connected_peers().await.unwrap().is_empty(),
        "b recorded an untrusted peer as connected"
    );
}

/// Trust is directional, and the listener enforces its own.
///
/// Written separately from the test above because a gate that only ran on the
/// dialer would pass that one: A refuses to finish, and both sides end up
/// empty. Here A trusts B and B does not trust A, so the only thing that can
/// keep B's peer list empty is B applying its own directory to an inbound
/// handshake.
#[tokio::test]
async fn the_listener_refuses_a_peer_it_does_not_trust_itself() {
    let a = spawn_node(model()).await;
    let b = spawn_node(model()).await;
    a.trust(b.local_peer_id()).await; // one direction only
    let b_addr = wait_for_listen_address(&b).await;

    let _ = a.connect(b.local_peer_id(), b_addr).await;

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        b.connected_peers().await.unwrap().is_empty(),
        "b accepted an inbound handshake from a peer it never trusted -- the \
         directory is only being consulted on the dialing side"
    );
}

/// Revoking trust means the peer cannot come back.
///
/// The handshake carries no nonce and no timestamp, so it is a static blob
/// that is byte-identical on every connection -- and every peer this Pond has
/// ever dialled holds a copy. If the transport did not re-consult the
/// directory, a revoked peer could reconnect by replaying the exchange it
/// already knows how to complete, and revocation would be advisory.
#[tokio::test]
async fn a_revoked_peer_cannot_reconnect() {
    let a = spawn_node(model()).await;
    let b = spawn_node(model()).await;
    trust_each_other(&a, &b).await;
    let b_addr = wait_for_listen_address(&b).await;

    a.connect(b.local_peer_id(), b_addr.clone()).await.unwrap();
    assert_eq!(a.connected_peers().await.unwrap(), vec![b.local_peer_id()]);

    // B throws A out of the circle.
    b.directory
        .remove_trusted_peer(a.local_peer_id())
        .await
        .unwrap();

    let a2 = spawn_node(model()).await;
    // A2 stands in for A reconnecting from a fresh process: it is trusted BY
    // nobody on B's side, exactly as A now is.
    a2.trust(b.local_peer_id()).await;
    let result = a2.connect(b.local_peer_id(), b_addr).await;
    assert!(
        result.is_err(),
        "a peer outside b's directory reconnected after revocation"
    );
}
