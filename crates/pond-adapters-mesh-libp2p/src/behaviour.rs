//! The composed libp2p `NetworkBehaviour` for a mesh node, and the
//! request/response message shapes carried over it.
//!
//! Scoped to what Circle-only trust actually needs (see the module docs on
//! each field below) — no public/OpenLane discovery anywhere in this stack.

use libp2p::swarm::NetworkBehaviour;
use libp2p::{dcutr, gossipsub, identify, kad, relay, request_response};
use serde::{Deserialize, Serialize};

/// The one request/response protocol a mesh node speaks: a `Handshake` right
/// after a connection opens (dialer -> listener), and opaque `Frame`s for
/// `MeshTransport::send`/`recv` traffic once that handshake is accepted.
pub const MESH_PROTOCOL: &str = "/pond-mesh/1.0.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MeshRequest {
    /// A prost-encoded `pond_mesh_protocol::wire::Handshake`.
    Handshake(Vec<u8>),
    /// An opaque application frame, only accepted from peers whose handshake
    /// already completed.
    Frame(Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MeshResponse {
    /// Carries the RESPONDER's own prost-encoded `Handshake`, so the exchange
    /// authenticates both directions.
    ///
    /// It used to be a bare unit variant, and that made the acceptance
    /// criterion one-sided: the dialer sent its handshake, the listener
    /// checked it, and the dialer then trusted a peer whose harness hash,
    /// model hash and key it had never seen. "A peer presenting a mismatched
    /// harness or model hash is refused" held in one direction only, and the
    /// direction it did not hold in is the one where we hand out frames.
    HandshakeAccepted(Vec<u8>),
    /// Harness/model hash mismatch, a signature that doesn't verify, an
    /// identity that doesn't match the connection, or a peer this Pond does
    /// not trust.
    HandshakeRejected,
    FrameAck,
}

#[derive(NetworkBehaviour)]
pub struct MeshBehaviour {
    /// Required alongside `dcutr` — DCUtR needs observed addresses `identify`
    /// collects to attempt a hole punch.
    pub identify: identify::Behaviour,
    /// Not used for open peer discovery. Used as an address-rendezvous DHT:
    /// a peer publishes `hash(own PeerId) -> current multiaddrs` so other
    /// Circle members can re-locate it after an IP/port change. Never
    /// queried for an arbitrary/unknown id.
    pub kad: kad::Behaviour<kad::store::MemoryStore>,
    /// One topic per Circle, used only for presence/address-announce
    /// broadcasts — not exposed on the `MeshTransport` port, internal
    /// plumbing only.
    pub gossipsub: gossipsub::Behaviour,
    /// So this node can relay for peers it's already connected to — no
    /// dedicated bootstrap/relay server in v1, any reachable Circle member
    /// can relay for another.
    pub relay: relay::Behaviour,
    /// So this node can use another connected peer as a relay when it can't
    /// dial a target directly.
    pub relay_client: relay::client::Behaviour,
    /// Attempts to upgrade a relayed connection to a direct one.
    pub dcutr: dcutr::Behaviour,
    pub mesh_rr: request_response::cbor::Behaviour<MeshRequest, MeshResponse>,
}
