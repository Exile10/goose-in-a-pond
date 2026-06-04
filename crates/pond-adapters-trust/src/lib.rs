//! SqliteTrustVerifier — Ed25519 signature verification with replay
//! protection, persisted in the same SQLite system DB the rest of the pond
//! uses.
//!
//! The verifier is the cryptographic boundary between the pond and the
//! GOTG phone. It owns:
//!
//!   * `device_pubkeys` — the canonical mapping from `install_id` to the
//!     phone's hardware-bound Ed25519 public key.
//!   * `replay_nonces` — every nonce ever accepted within the replay
//!     window, used to reject double-spends.
//!
//! It is the *only* code that calls Ed25519 verify; every other layer
//! goes through the [`TrustVerifier`] trait.
//!
//! See [`pond_core::domain::trust::signature_scope`] for the canonical
//! message format.

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use pond_core::domain::trust::{
    signature_scope, Intent, SignedAssertion, TrustError,
};
use pond_core::ports::trust::TrustVerifier;
use sha2::{Digest, Sha256};
use sqlx::{Pool, Sqlite};
use tracing::{debug, info, warn};

/// Replay window. Assertions whose `ts` is older than this are rejected as
/// stale; assertions whose nonce was already seen within this window are
/// rejected as replayed. Override at construction time.
pub const DEFAULT_REPLAY_WINDOW_SECS: i64 = 300; // 5 minutes

/// How aggressively `prune_replay` deletes old rows. Slightly larger than the
/// window so a nonce that just timed out can't be re-used by an adversarial
/// retry that races the prune.
pub const DEFAULT_PRUNE_BEYOND_SECS: i64 = 600; // 10 minutes

/// Maximum allowed clock skew between the phone's `ts` and the pond's
/// wall clock. Phones generally have NTP-synced clocks; 30 s is generous.
pub const DEFAULT_MAX_SKEW_SECS: i64 = 30;

pub struct SqliteTrustVerifier {
    pool:                  Pool<Sqlite>,
    replay_window_secs:    i64,
    prune_beyond_secs:     i64,
    max_skew_secs:         i64,
}

impl SqliteTrustVerifier {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self {
            pool,
            replay_window_secs: DEFAULT_REPLAY_WINDOW_SECS,
            prune_beyond_secs:  DEFAULT_PRUNE_BEYOND_SECS,
            max_skew_secs:      DEFAULT_MAX_SKEW_SECS,
        }
    }

    pub fn with_replay_window(mut self, secs: i64) -> Self {
        self.replay_window_secs = secs;
        self.prune_beyond_secs  = (secs * 2).max(secs + 60);
        self
    }

    pub fn with_max_skew(mut self, secs: i64) -> Self {
        self.max_skew_secs = secs;
        self
    }

    /// Read the public key for `install_id`. Returns `None` when no key is
    /// registered. Returns `Some(Err(Revoked))` when the key exists but
    /// was revoked. Returns `Some(Err(BadSignature))` when the stored bytes
    /// do not decode as an Ed25519 public key.
    async fn lookup_pubkey(
        &self,
        install_id: &str,
    ) -> Result<Option<VerifyingKey>, TrustError> {
        let row: Option<(Vec<u8>, Option<String>)> = sqlx::query_as(
            "SELECT public_key, revoked_at FROM device_pubkeys \
             WHERE install_id = ?",
        )
        .bind(install_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| TrustError::Storage(e.into()))?;

        let Some((bytes, revoked_at)) = row else { return Ok(None); };
        if revoked_at.is_some() {
            return Err(TrustError::Revoked(install_id.to_string()));
        }
        let key_bytes: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| TrustError::BadSignature)?;
        VerifyingKey::from_bytes(&key_bytes)
            .map(Some)
            .map_err(|_| TrustError::BadSignature)
    }
}

#[async_trait]
impl TrustVerifier for SqliteTrustVerifier {
    async fn register_pubkey(
        &self,
        install_id: &str,
        public_key: [u8; 32],
    ) -> anyhow::Result<()> {
        // Validate it decodes as an Ed25519 key before storing — refusing
        // garbage at the boundary keeps `lookup_pubkey` total.
        VerifyingKey::from_bytes(&public_key)
            .context("rejected: not a valid Ed25519 public key")?;

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO device_pubkeys \
                 (install_id, public_key, algorithm, registered_at, last_used_at, revoked_at) \
             VALUES (?, ?, 'ed25519', ?, NULL, NULL) \
             ON CONFLICT(install_id) DO UPDATE SET \
                 public_key    = excluded.public_key, \
                 algorithm     = 'ed25519', \
                 registered_at = excluded.registered_at, \
                 revoked_at    = NULL",
        )
        .bind(install_id)
        .bind(public_key.to_vec())
        .bind(&now)
        .execute(&self.pool)
        .await
        .context("device_pubkeys upsert failed")?;
        info!(%install_id, "registered Ed25519 public key");
        Ok(())
    }

    async fn revoke_pubkey(&self, install_id: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let res = sqlx::query(
            "UPDATE device_pubkeys SET revoked_at = ? \
             WHERE install_id = ? AND revoked_at IS NULL",
        )
        .bind(&now)
        .bind(install_id)
        .execute(&self.pool)
        .await
        .context("device_pubkeys revoke failed")?;
        info!(%install_id, rows = res.rows_affected(), "revoked Ed25519 public key");
        Ok(())
    }

    async fn verify_assertion(
        &self,
        intent:    &Intent,
        assertion: &SignedAssertion,
    ) -> Result<(), TrustError> {
        // ── Gate 1: intent expiry ─────────────────────────────────────────
        let now = Utc::now();
        if now > intent.expires_at {
            return Err(TrustError::ExpiredIntent);
        }

        // ── Gate 2: timestamp skew ────────────────────────────────────────
        let skew = (now - assertion.ts).num_seconds().abs();
        if skew > self.max_skew_secs {
            return Err(TrustError::StaleTimestamp { skew_secs: skew });
        }

        // ── Gate 3: public-key lookup ─────────────────────────────────────
        let key = self
            .lookup_pubkey(&assertion.install_id)
            .await?
            .ok_or_else(|| TrustError::UnknownDevice(assertion.install_id.clone()))?;

        // ── Gate 4: signature verification ────────────────────────────────
        let scope = signature_scope(
            &assertion.install_id,
            intent.id,
            &intent.action,
            &intent.payload_hash,
            assertion.ts,
            &assertion.nonce,
        );
        let sig = Signature::from_bytes(&assertion.signature);
        key.verify(&hash_for_signing(&scope), &sig)
            .map_err(|_| TrustError::BadSignature)?;

        // ── Gate 5 (atomic): nonce uniqueness + record ────────────────────
        // Insert with PK conflict → replayed. Any other DB error bubbles up.
        let nonce_blob = assertion.nonce.to_vec();
        let seen_at = assertion.ts.to_rfc3339();
        let res = sqlx::query(
            "INSERT INTO replay_nonces (install_id, nonce, seen_at) \
             VALUES (?, ?, ?)",
        )
        .bind(&assertion.install_id)
        .bind(&nonce_blob)
        .bind(&seen_at)
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => {}
            Err(sqlx::Error::Database(db)) if is_unique_violation(db.as_ref()) => {
                debug!(
                    install_id = %assertion.install_id,
                    "rejected: nonce already seen"
                );
                return Err(TrustError::ReplayedNonce);
            }
            Err(e) => return Err(TrustError::Storage(e.into())),
        }

        // Touch last_used_at — best-effort.
        let touch_now = Utc::now().to_rfc3339();
        if let Err(e) = sqlx::query(
            "UPDATE device_pubkeys SET last_used_at = ? WHERE install_id = ?",
        )
        .bind(&touch_now)
        .bind(&assertion.install_id)
        .execute(&self.pool)
        .await
        {
            warn!(install_id = %assertion.install_id, "last_used_at touch failed: {e:#}");
        }

        info!(
            install_id = %assertion.install_id,
            intent_id = %intent.id,
            action = %intent.action,
            "biometric assertion verified"
        );
        Ok(())
    }

    async fn prune_replay(&self) -> anyhow::Result<u64> {
        let cutoff: DateTime<Utc> =
            Utc::now() - ChronoDuration::seconds(self.prune_beyond_secs);
        let res = sqlx::query("DELETE FROM replay_nonces WHERE seen_at < ?")
            .bind(cutoff.to_rfc3339())
            .execute(&self.pool)
            .await
            .context("replay_nonces prune failed")?;
        Ok(res.rows_affected())
    }
}

/// Hash the canonical scope before passing to Ed25519 verify.
///
/// `pond_core::domain::trust::signature_scope` already emits a length-
/// prefixed buffer prefixed with `"giap-intent-v1\0"`; we hash with SHA-256
/// once before signing/verifying so the on-wire signature is over a fixed
/// 32-byte digest — a defensive measure against accidental signing of an
/// over-long buffer.
pub(crate) fn hash_for_signing(scope: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(scope);
    hasher.finalize().into()
}

/// SQLite primary-key collision detection. The error code is "1555" for
/// PRIMARY KEY conflicts and "2067" for UNIQUE conflicts; we accept either
/// because behaviour can vary across SQLite versions and binding shapes.
fn is_unique_violation(err: &dyn sqlx::error::DatabaseError) -> bool {
    let code = err.code().unwrap_or_default();
    matches!(code.as_ref(), "1555" | "2067" | "19")
        || err.message().contains("UNIQUE constraint failed")
}

pub mod intent_bus;
pub use intent_bus::InMemoryIntentBus;

#[cfg(test)]
mod tests;
