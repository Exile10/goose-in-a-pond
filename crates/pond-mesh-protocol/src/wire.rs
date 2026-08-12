//! Wire messages exchanged between mesh peers (#132).
//!
//! Fields are derived directly on plain Rust structs via `prost::Message` —
//! no `.proto` file and no `protoc`/build-time codegen, which keeps this
//! crate free of an extra native build tool (the Jetson cross-build already
//! has to work around ggml's cmake probing; this crate deliberately doesn't
//! add a second one).
//!
//! `Handshake` is the one message this milestone defines: it's what a peer
//! presents on `MeshTransport::connect` so the receiving side can check the
//! harness/model hash pin (an #132 acceptance criterion) before trusting the
//! connection. Additional message types (inference shard requests, etc.)
//! are deferred to the milestone that actually needs them.

use prost::Message;

use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};
use pond_core::mesh::domain::peer_id::PeerId;

#[derive(Clone, PartialEq, Eq, Message)]
pub struct Handshake {
    #[prost(bytes = "vec", tag = "1")]
    pub peer_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub harness_hash: Vec<u8>,
    #[prost(bytes = "vec", tag = "3")]
    pub model_hash: Vec<u8>,
    /// ed25519 signature (64 bytes) over `peer_id || harness_hash || model_hash`.
    #[prost(bytes = "vec", tag = "4")]
    pub signature: Vec<u8>,
}

impl Handshake {
    pub fn new(peer: PeerId, harness: HarnessHash, model: ModelHash, signature: [u8; 64]) -> Self {
        Self {
            peer_id: peer.as_bytes().to_vec(),
            harness_hash: harness.as_bytes().to_vec(),
            model_hash: model.as_bytes().to_vec(),
            signature: signature.to_vec(),
        }
    }

    /// The bytes a signer/verifier should sign/check over — deterministic
    /// concatenation of the three identity fields, excluding the signature
    /// itself.
    pub fn signed_payload(&self) -> Vec<u8> {
        [
            &self.peer_id[..],
            &self.harness_hash[..],
            &self.model_hash[..],
        ]
        .concat()
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    pub fn decode(buf: &[u8]) -> Result<Self, prost::DecodeError> {
        Message::decode(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::MeshKeypair;

    fn sample_handshake() -> (Handshake, MeshKeypair) {
        let keypair = MeshKeypair::generate();
        let harness = HarnessHash::from([1u8; 32]);
        let model = ModelHash::from([2u8; 32]);
        let unsigned = Handshake::new(keypair.peer_id(), harness, model, [0u8; 64]);
        let signature = keypair.sign(&unsigned.signed_payload());
        (
            Handshake::new(keypair.peer_id(), harness, model, signature),
            keypair,
        )
    }

    #[test]
    fn encode_decode_roundtrips() {
        let (handshake, _keypair) = sample_handshake();
        let bytes = handshake.encode_to_vec();
        let decoded = Handshake::decode(&bytes[..]).unwrap();
        assert_eq!(handshake, decoded);
    }

    #[test]
    fn signature_verifies_against_signed_payload() {
        let (handshake, keypair) = sample_handshake();
        let signature: [u8; 64] = handshake.signature.clone().try_into().unwrap();
        assert!(crate::identity::verify(
            keypair.peer_id(),
            &handshake.signed_payload(),
            &signature
        )
        .unwrap());
    }

    #[test]
    fn decode_of_garbage_bytes_errors_not_panics() {
        // A handful of random bytes are very unlikely to be a valid encoding,
        // but even if a length happens to parse, decode() must never panic —
        // this is exactly the "refuse a malformed peer" path.
        let result = Handshake::decode(&[0xff, 0x00, 0x01][..]);
        let _ = result; // either Ok or Err is acceptable; a panic is not.
    }
}
