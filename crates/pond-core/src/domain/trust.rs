//! Biometric trust domain types.
//!
//! Three classes of action:
//!   * **Ambient** — read-only / non-personal. No identity required.
//!   * **Personal** — user-scoped reads/writes. Requires a valid session.
//!   * **Privileged** — irreversible / high-risk. Requires a hardware-attested
//!     biometric assertion from a paired phone.
//!
//! The pond never gates Privileged actions on its own webcam-based face
//! recognition because that pipeline cannot be made photo-proof on commodity
//! hardware. Instead the pond *publishes an intent* and waits for a paired
//! GOTG device to sign the challenge with its hardware-bound key — gated
//! by Face ID / fingerprint at the OS level. See `docs/biometric-trust-system.md`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Domain types intentionally avoid #[derive(Serialize, Deserialize)] for the
// byte-array fields (nonce, signature, payload_hash). The wire format lives
// in the API/WebSocket adapter, which serialises these as hex strings — the
// domain stays pure Rust and uuid/array-serde feature flags don't have to
// propagate through the workspace.

/// Trust level required to execute a given action.
///
/// Ordered such that `Ambient < Personal < Privileged`, so callers can
/// compare with `>=` to ask "does this credential clear the bar?".
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Ambient,
    Personal,
    Privileged,
}

impl TrustLevel {
    pub fn requires_session(self) -> bool {
        matches!(self, Self::Personal | Self::Privileged)
    }

    pub fn requires_biometric_assertion(self) -> bool {
        matches!(self, Self::Privileged)
    }
}

/// Whether an action may be invoked from a remote (non-LAN) origin.
///
/// Some Privileged actions — physical-world side effects like "unlock front
/// door" — should never execute remotely no matter how strong the biometric
/// proof, because a coerced biometric event still looks legitimate to the
/// signing key. `LocalOnly` actions are refused with a clear error when the
/// request did not arrive over the LAN.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RemotePolicy {
    Allowed,
    LocalOnly,
}

/// A canonical pending action waiting for a biometric assertion.
///
/// The pond creates an Intent when a Privileged-tier handler fires, pushes
/// it to all paired phones over the auth WebSocket, and blocks (with a
/// timeout) until a matching `SignedAssertion` arrives.
#[derive(Debug, Clone)]
pub struct Intent {
    pub id:           Uuid,
    /// Stable canonical action name (e.g. `"settings.update_wake_word"`).
    /// This is part of the signature scope, so renames are breaking.
    pub action:       String,
    /// Short human-readable summary the phone shows in its approval prompt.
    pub summary:      String,
    /// blake3 of the canonicalised request payload. Bound into the signature
    /// so a phone that approved one payload cannot be replayed against a
    /// different payload.
    pub payload_hash: [u8; 32],
    /// Origin: an install_id, or `"local-desktop"` for desktop-app intents.
    pub requested_by: String,
    pub created_at:   DateTime<Utc>,
    pub expires_at:   DateTime<Utc>,
}

/// A biometric assertion from a paired GOTG device.
///
/// The pond verifies the `signature` against the phone's stored public key,
/// over the canonical signature scope:
/// ```text
/// SHA256("giap-intent-v1\0" || install_id || intent_id || action ||
///        payload_hash || ts || nonce)
/// ```
/// then checks `nonce` is unseen and `ts` is within the replay window.
#[derive(Debug, Clone)]
pub struct SignedAssertion {
    pub intent_id:  Uuid,
    pub install_id: String,
    /// Phone's wall-clock at signing time. ±30 s skew tolerance.
    pub ts:         DateTime<Utc>,
    /// 16-byte random nonce, distinct per assertion.
    pub nonce:      [u8; 16],
    /// 64-byte Ed25519 signature.
    pub signature:  [u8; 64],
}

/// Outcome of a pending intent.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum IntentOutcome {
    Approved,
    Denied,
    Expired,
}

/// Errors a `TrustVerifier` can return.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("no public key registered for install_id {0}")]
    UnknownDevice(String),
    #[error("public key for {0} has been revoked")]
    Revoked(String),
    #[error("signature verification failed")]
    BadSignature,
    #[error("timestamp out of replay window (skew {skew_secs} s)")]
    StaleTimestamp { skew_secs: i64 },
    #[error("nonce already seen")]
    ReplayedNonce,
    #[error("action mismatch (signed for {signed}, requested {requested})")]
    ActionMismatch {
        signed:    String,
        requested: String,
    },
    #[error("intent expired")]
    ExpiredIntent,
    #[error("storage error: {0}")]
    Storage(#[from] anyhow::Error),
}

/// Domain-separated signature scope. Single source of truth for what
/// constitutes "the message that was signed" — used by both the pond
/// verifier and the GOTG signer (kept identical via test fixtures).
pub fn signature_scope(
    install_id:   &str,
    intent_id:    Uuid,
    action:       &str,
    payload_hash: &[u8; 32],
    ts:           DateTime<Utc>,
    nonce:        &[u8; 16],
) -> Vec<u8> {
    // SHA256-domain-separated payload.
    //
    // We use a fixed prefix `"giap-intent-v1\0"` so this signature can never
    // be confused with a future protocol revision or a different message
    // type (e.g. a session-refresh signature). Every field is length-
    // delimited where its width isn't fixed.
    let mut buf = Vec::with_capacity(256);
    buf.extend_from_slice(b"giap-intent-v1\0");
    push_lp(&mut buf, install_id.as_bytes());
    buf.extend_from_slice(intent_id.as_bytes());
    push_lp(&mut buf, action.as_bytes());
    buf.extend_from_slice(payload_hash);
    let ts_secs = ts.timestamp();
    buf.extend_from_slice(&ts_secs.to_be_bytes());
    buf.extend_from_slice(nonce);
    buf
}

fn push_lp(buf: &mut Vec<u8>, slice: &[u8]) {
    let len: u32 = slice.len() as u32;
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(slice);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_level_ordering() {
        assert!(TrustLevel::Privileged > TrustLevel::Personal);
        assert!(TrustLevel::Personal > TrustLevel::Ambient);
    }

    #[test]
    fn signature_scope_is_deterministic() {
        let id = Uuid::nil();
        let ts = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let nonce = [0u8; 16];
        let payload = [0u8; 32];
        let a = signature_scope("install-1", id, "settings.x", &payload, ts, &nonce);
        let b = signature_scope("install-1", id, "settings.x", &payload, ts, &nonce);
        assert_eq!(a, b);
    }

    #[test]
    fn signature_scope_is_field_separated() {
        // Differing only in field boundary placement must not collide.
        let id = Uuid::nil();
        let ts = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let nonce = [0u8; 16];
        let payload = [0u8; 32];
        let a = signature_scope("install", id, "1settings.x", &payload, ts, &nonce);
        let b = signature_scope("install1", id, "settings.x", &payload, ts, &nonce);
        assert_ne!(a, b, "length-prefixing must prevent boundary ambiguity");
    }
}
