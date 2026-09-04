//! Bridges our domain [`PeerId`] (a raw 32-byte ed25519 public key) and
//! [`pond_mesh_protocol::identity::MeshKeypair`] to libp2p's own identity types.
//! Both are deterministic functions of the same ed25519 key material, so a Pond's
//! mesh identity is one key wrapped two ways.

use libp2p::identity::{ed25519, Keypair, PublicKey};
use libp2p::PeerId as Libp2pPeerId;
use thiserror::Error;

use pond_core::mesh::domain::peer_id::PeerId;
use pond_mesh_protocol::identity::MeshKeypair;

#[derive(Error, Debug)]
pub enum IdentityBridgeError {
    #[error("malformed peer id: {0}")]
    MalformedPeerId(String),
}

/// Reconstruct an equivalent libp2p keypair from the same secret bytes our
/// `Handshake` messages sign with, so the swarm authenticates as the same
/// identity.
pub fn to_libp2p_keypair(keypair: &MeshKeypair) -> Keypair {
    Keypair::ed25519_from_bytes(keypair.secret_bytes())
        .expect("a 32-byte ed25519 secret is always a valid libp2p keypair")
}

/// Derive the libp2p `PeerId` a caller must dial to reach a given domain
/// `PeerId` — used every time the port hands us a domain peer and we need to
/// talk to the `Swarm`.
pub fn domain_peer_to_libp2p(peer: PeerId) -> Result<Libp2pPeerId, IdentityBridgeError> {
    let ed_public = ed25519::PublicKey::try_from_bytes(peer.as_bytes())
        .map_err(|err| IdentityBridgeError::MalformedPeerId(err.to_string()))?;
    let public: PublicKey = ed_public.into();
    Ok(public.to_peer_id())
}

/// Recover our domain `PeerId` from a public key libp2p handed us (e.g. from
/// an `identify` event) — not from the libp2p `PeerId` itself, since
/// recovering key bytes from a bare `PeerId` isn't a supported operation.
pub fn public_key_to_domain(public: &PublicKey) -> Option<PeerId> {
    let ed_public = public.clone().try_into_ed25519().ok()?;
    Some(PeerId::from(ed_public.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_peer_and_public_key_roundtrip() {
        let mesh_keypair = MeshKeypair::generate();
        let libp2p_keypair = to_libp2p_keypair(&mesh_keypair);
        let recovered = public_key_to_domain(&libp2p_keypair.public()).unwrap();
        assert_eq!(recovered, mesh_keypair.peer_id());
    }

    #[test]
    fn domain_peer_to_libp2p_matches_keypair_peer_id() {
        let mesh_keypair = MeshKeypair::generate();
        let libp2p_keypair = to_libp2p_keypair(&mesh_keypair);
        let derived = domain_peer_to_libp2p(mesh_keypair.peer_id()).unwrap();
        assert_eq!(derived, libp2p_keypair.public().to_peer_id());
    }
}
