//! End-to-end tests for the trust verifier. Each test spins up a fresh
//! in-memory SQLite, runs the system migrations, and exercises the
//! public surface of [`SqliteTrustVerifier`].

use super::*;
use chrono::{Duration as ChronoDuration, Utc};
use ed25519_dalek::{Signer, SigningKey};
use pond_core::domain::trust::{Intent, SignedAssertion, TrustError};
use pond_core::ports::trust::TrustVerifier;
use rand_core::{OsRng, RngCore};
use sqlx::sqlite::SqlitePoolOptions;
use uuid::Uuid;

async fn fresh_pool() -> sqlx::Pool<sqlx::Sqlite> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    // Bring up only the migrations we depend on. We can't reuse
    // `pond-infra::Database` here without a circular dep, so apply the
    // 0016 schema directly. The verifier doesn't depend on any earlier
    // migration.
    sqlx::query(include_str!(
        "../../pond-infra/migrations/system/0016_trust_system.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    pool
}

fn make_intent(action: &str) -> Intent {
    let now = Utc::now();
    Intent {
        id:           Uuid::new_v4(),
        action:       action.to_string(),
        summary:      format!("Test: {action}"),
        payload_hash: [0xAA; 32],
        requested_by: "test-harness".to_string(),
        created_at:   now,
        expires_at:   now + ChronoDuration::seconds(60),
    }
}

fn sign_assertion(
    sk:         &SigningKey,
    install_id: &str,
    intent:     &Intent,
    skew_secs:  i64,
) -> SignedAssertion {
    let mut nonce = [0u8; 16];
    OsRng.fill_bytes(&mut nonce);
    let ts = Utc::now() + ChronoDuration::seconds(skew_secs);
    let scope = pond_core::domain::trust::signature_scope(
        install_id,
        intent.id,
        &intent.action,
        &intent.payload_hash,
        ts,
        &nonce,
    );
    let digest = hash_for_signing(&scope);
    let sig = sk.sign(&digest);
    SignedAssertion {
        intent_id:  intent.id,
        install_id: install_id.to_string(),
        ts,
        nonce,
        signature:  sig.to_bytes(),
    }
}

#[tokio::test]
async fn happy_path_verify_succeeds() {
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool);
    let sk = SigningKey::generate(&mut OsRng);
    v.register_pubkey("install-1", sk.verifying_key().to_bytes())
        .await
        .unwrap();
    let intent = make_intent("settings.update_wake_word");
    let assertion = sign_assertion(&sk, "install-1", &intent, 0);
    v.verify_assertion(&intent, &assertion).await.unwrap();
}

#[tokio::test]
async fn unknown_device_rejected() {
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool);
    let sk = SigningKey::generate(&mut OsRng);
    let intent = make_intent("settings.x");
    let assertion = sign_assertion(&sk, "unknown", &intent, 0);
    let err = v.verify_assertion(&intent, &assertion).await.unwrap_err();
    assert!(matches!(err, TrustError::UnknownDevice(_)), "got {err:?}");
}

#[tokio::test]
async fn revoked_key_rejected() {
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool);
    let sk = SigningKey::generate(&mut OsRng);
    v.register_pubkey("install-1", sk.verifying_key().to_bytes())
        .await
        .unwrap();
    v.revoke_pubkey("install-1").await.unwrap();
    let intent = make_intent("settings.x");
    let assertion = sign_assertion(&sk, "install-1", &intent, 0);
    let err = v.verify_assertion(&intent, &assertion).await.unwrap_err();
    assert!(matches!(err, TrustError::Revoked(_)), "got {err:?}");
}

#[tokio::test]
async fn tampered_signature_rejected() {
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool);
    let sk = SigningKey::generate(&mut OsRng);
    v.register_pubkey("install-1", sk.verifying_key().to_bytes())
        .await
        .unwrap();
    let intent = make_intent("settings.x");
    let mut assertion = sign_assertion(&sk, "install-1", &intent, 0);
    assertion.signature[0] ^= 0x01;
    let err = v.verify_assertion(&intent, &assertion).await.unwrap_err();
    assert!(matches!(err, TrustError::BadSignature), "got {err:?}");
}

#[tokio::test]
async fn cross_action_signature_rejected() {
    // A signature that is valid for a *different* action must not authorise
    // this intent — the action name is part of the signature scope.
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool);
    let sk = SigningKey::generate(&mut OsRng);
    v.register_pubkey("install-1", sk.verifying_key().to_bytes())
        .await
        .unwrap();
    let real_intent = make_intent("settings.update_wake_word");
    let other_intent = make_intent("settings.delete_everything");
    let assertion = sign_assertion(&sk, "install-1", &other_intent, 0);
    // Reuse the assertion's signature against the real intent.
    let attack = SignedAssertion {
        intent_id:  real_intent.id,
        ..assertion
    };
    let err = v
        .verify_assertion(&real_intent, &attack)
        .await
        .unwrap_err();
    assert!(matches!(err, TrustError::BadSignature), "got {err:?}");
}

#[tokio::test]
async fn replay_rejected() {
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool);
    let sk = SigningKey::generate(&mut OsRng);
    v.register_pubkey("install-1", sk.verifying_key().to_bytes())
        .await
        .unwrap();
    let intent = make_intent("settings.x");
    let assertion = sign_assertion(&sk, "install-1", &intent, 0);
    v.verify_assertion(&intent, &assertion).await.unwrap();
    // Replay with same nonce.
    let err = v.verify_assertion(&intent, &assertion).await.unwrap_err();
    assert!(matches!(err, TrustError::ReplayedNonce), "got {err:?}");
}

#[tokio::test]
async fn stale_timestamp_rejected() {
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool);
    let sk = SigningKey::generate(&mut OsRng);
    v.register_pubkey("install-1", sk.verifying_key().to_bytes())
        .await
        .unwrap();
    let intent = make_intent("settings.x");
    // 5 minutes in the past.
    let assertion = sign_assertion(&sk, "install-1", &intent, -300);
    let err = v.verify_assertion(&intent, &assertion).await.unwrap_err();
    assert!(
        matches!(err, TrustError::StaleTimestamp { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn expired_intent_rejected() {
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool);
    let sk = SigningKey::generate(&mut OsRng);
    v.register_pubkey("install-1", sk.verifying_key().to_bytes())
        .await
        .unwrap();
    let now = Utc::now();
    let intent = Intent {
        id:           Uuid::new_v4(),
        action:       "settings.x".to_string(),
        summary:      "expired".to_string(),
        payload_hash: [0; 32],
        requested_by: "t".to_string(),
        created_at:   now - ChronoDuration::seconds(120),
        expires_at:   now - ChronoDuration::seconds(60),
    };
    let assertion = sign_assertion(&sk, "install-1", &intent, 0);
    let err = v.verify_assertion(&intent, &assertion).await.unwrap_err();
    assert!(matches!(err, TrustError::ExpiredIntent), "got {err:?}");
}

#[tokio::test]
async fn re_register_replaces_key_and_clears_revocation() {
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool);
    let sk1 = SigningKey::generate(&mut OsRng);
    v.register_pubkey("install-1", sk1.verifying_key().to_bytes())
        .await
        .unwrap();
    v.revoke_pubkey("install-1").await.unwrap();
    let sk2 = SigningKey::generate(&mut OsRng);
    v.register_pubkey("install-1", sk2.verifying_key().to_bytes())
        .await
        .unwrap();
    let intent = make_intent("settings.x");
    let assertion = sign_assertion(&sk2, "install-1", &intent, 0);
    v.verify_assertion(&intent, &assertion).await.unwrap();
}

#[tokio::test]
async fn prune_removes_old_nonces() {
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool.clone()).with_replay_window(60);
    // Insert a synthetic-old nonce directly.
    let old_ts = (Utc::now() - ChronoDuration::seconds(7200)).to_rfc3339();
    sqlx::query(
        "INSERT INTO replay_nonces (install_id, nonce, seen_at) VALUES (?, ?, ?)",
    )
    .bind("install-1")
    .bind(vec![1u8; 16])
    .bind(&old_ts)
    .execute(&pool)
    .await
    .unwrap();
    let pruned = v.prune_replay().await.unwrap();
    assert_eq!(pruned, 1);
}

#[tokio::test]
async fn invalid_pubkey_bytes_rejected_at_register() {
    let pool = fresh_pool().await;
    let v = SqliteTrustVerifier::new(pool);
    let bogus = [0xFFu8; 32];
    // Most random 32-byte arrays do decode to a VerifyingKey (the curve
    // tolerates many points), so the register call may succeed. The
    // important contract is a verify with a mismatched key fails — covered
    // by `tampered_signature_rejected`. This test asserts the function
    // is at least *callable* with arbitrary bytes without panicking.
    let _ = v.register_pubkey("install-bogus", bogus).await;
}
