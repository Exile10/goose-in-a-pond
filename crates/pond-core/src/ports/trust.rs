//! TrustVerifier port — verifies hardware-attested biometric assertions
//! from paired GOTG devices.
//!
//! See [`crate::domain::trust`] for the domain types and the canonical
//! signature scope.

use crate::domain::trust::{Intent, SignedAssertion, TrustError};
use anyhow::Result;
use async_trait::async_trait;

/// Driven port: verify an Ed25519 signed assertion against a stored device
/// public key, with replay protection.
///
/// The "successful verification" contract is intentionally narrow:
///
///   1. The signature decodes and verifies under the install_id's stored
///      public key.
///   2. The signed `action` matches the requested `intent.action` byte-for-byte.
///   3. The signed `payload_hash` matches the requested `intent.payload_hash`.
///   4. The timestamp is within ±replay_window_secs of the verifier's clock.
///   5. The (install_id, nonce) tuple has not been seen in the replay window.
///   6. The intent has not expired.
///   7. The public key is registered and not revoked.
///
/// Any failure surfaces as a typed [`TrustError`] so callers can map
/// 401/403/408/410 status codes appropriately.
#[async_trait]
pub trait TrustVerifier: Send + Sync {
    /// Register a new public key for `install_id`.  Idempotent — replaces
    /// any prior key for that install_id (re-pair flow).  Resets `revoked_at`.
    async fn register_pubkey(
        &self,
        install_id: &str,
        public_key: [u8; 32],
    ) -> Result<()>;

    /// Mark the public key for `install_id` as revoked.  Subsequent
    /// `verify_assertion` calls will return [`TrustError::Revoked`].
    async fn revoke_pubkey(&self, install_id: &str) -> Result<()>;

    /// Verify the assertion against the intent and the stored public key,
    /// recording the nonce on success so a replay would fail.
    ///
    /// **Atomicity contract:** either every gate passes and the nonce is
    /// recorded, or the call fails and no state changes.
    async fn verify_assertion(
        &self,
        intent:    &Intent,
        assertion: &SignedAssertion,
    ) -> Result<(), TrustError>;

    /// Prune nonces older than the replay window.  Cheap; intended to be
    /// called from a periodic background task.
    async fn prune_replay(&self) -> Result<u64>;
}

/// Default no-op verifier used by tests / mocks that don't exercise the
/// trust path.  Always rejects with `BadSignature` to avoid accidentally
/// granting privileged access in a half-wired test fixture.
pub struct NullTrustVerifier;

#[async_trait]
impl TrustVerifier for NullTrustVerifier {
    async fn register_pubkey(&self, _: &str, _: [u8; 32]) -> Result<()> {
        Ok(())
    }
    async fn revoke_pubkey(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn verify_assertion(
        &self,
        _: &Intent,
        _: &SignedAssertion,
    ) -> Result<(), TrustError> {
        Err(TrustError::BadSignature)
    }
    async fn prune_replay(&self) -> Result<u64> {
        Ok(0)
    }
}
