//! Stable identifier for a mesh peer (#132).
//!
//! A 32-byte newtype (matches an ed25519 public key / libp2p peer key length)
//! rather than a bare `String` so a peer id can't be silently confused with
//! any other identifier at a call site.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId([u8; 32]);

impl PeerId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for PeerId {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_lowercase_hex() {
        let peer = PeerId::from([0xabu8; 32]);
        assert_eq!(peer.to_string(), "ab".repeat(32));
    }

    #[test]
    fn distinct_bytes_are_not_equal() {
        assert_ne!(PeerId::from([0u8; 32]), PeerId::from([1u8; 32]));
    }
}
