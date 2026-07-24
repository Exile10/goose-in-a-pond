//! Content hashing for mesh trust-pinning (#132).
//!
//! Produces the `HarnessHash`/`ModelHash` value types owned by `pond-core`'s
//! `mesh` domain — this module is the one place that actually runs blake3.

use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};

pub fn hash_harness(bytes: &[u8]) -> HarnessHash {
    HarnessHash::from(*blake3::hash(bytes).as_bytes())
}

pub fn hash_model(bytes: &[u8]) -> ModelHash {
    ModelHash::from(*blake3::hash(bytes).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_hash_identically() {
        assert_eq!(hash_harness(b"harness-v1"), hash_harness(b"harness-v1"));
    }

    #[test]
    fn different_bytes_hash_differently() {
        assert_ne!(hash_harness(b"harness-v1"), hash_harness(b"harness-v2"));
    }

    #[test]
    fn harness_and_model_hash_of_same_bytes_carry_same_digest() {
        // HarnessHash and ModelHash are distinct types, but nothing stops the
        // same bytes producing the same underlying digest in each — the type
        // system is what prevents mixing them up at a call site, not the hash.
        let bytes = b"some-weights";
        assert_eq!(hash_harness(bytes).as_bytes(), hash_model(bytes).as_bytes());
    }
}
