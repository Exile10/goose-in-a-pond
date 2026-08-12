//! Real `MeshTransport` for the private Pond Compute mesh (#132), built on
//! libp2p. `PeerDirectory` stays mocked — trust-pin persistence belongs to
//! the later `pond-infra` milestone, not networking.

pub mod adapter;
pub mod behaviour;
pub mod identity;
pub mod swarm_task;

pub use adapter::{Libp2pMeshTransport, Libp2pMeshTransportConfig};
