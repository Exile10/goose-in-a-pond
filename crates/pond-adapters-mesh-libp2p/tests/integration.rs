//! In-process, loopback-only integration tests for `Libp2pMeshTransport`.
//! Nothing here touches real hardware or the public network, so no test is
//! `#[ignore]`d.

use std::sync::Arc;
use std::time::Duration;

use pond_adapters_mesh_libp2p::identity::domain_peer_to_libp2p;
use pond_adapters_mesh_libp2p::{Libp2pMeshTransport, Libp2pMeshTransportConfig};
use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};
use pond_core::mesh::ports::mesh_transport::MeshTransport;
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

async fn spawn_node(model_hash: ModelHash) -> Libp2pMeshTransport {
    let config = Libp2pMeshTransportConfig {
        listen_addr: "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
        harness_hash: harness(),
        model_hash,
        keypair: MeshKeypair::generate(),
    };
    Libp2pMeshTransport::new(config).unwrap()
}

/// Poll `listen_addresses()` until at least one address is confirmed —
/// binding to `tcp/0` resolves the actual port asynchronously.
async fn wait_for_listen_address(node: &Libp2pMeshTransport) -> String {
    for _ in 0..200 {
        let addrs = node.listen_addresses().await;
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
    let _: Arc<dyn MeshTransport> = Arc::new(node);
}

#[tokio::test]
async fn connect_send_recv_roundtrip() {
    let a = spawn_node(model()).await;
    let b = spawn_node(model()).await;
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
