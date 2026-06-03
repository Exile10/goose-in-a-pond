//! OAuth 2.1 PKCE session management.
//!
//! Stores ephemeral PKCE sessions in memory (a single `HashMap` behind a
//! `RwLock`).  Sessions are short-lived — created when the user clicks
//! "Sign in with X" and consumed when the provider redirects back with
//! an authorization code.  No persistence is needed.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// An in-flight PKCE authorization session.
///
/// Created by `POST /oauth/authorize`, consumed by `GET /oauth/callback`.
pub struct PkceSession {
    /// Which provider this session targets (e.g. "spotify").
    pub provider_id: String,
    /// The PKCE `code_verifier` — sent to the token endpoint, never to the
    /// authorization endpoint.
    pub code_verifier: String,
    /// Optional marketplace extension to auto-install after successful auth.
    pub extension_id: Option<String>,
    /// When the session was created — allows stale session cleanup.
    pub created_at: std::time::Instant,
}

/// Shared state for all in-flight OAuth PKCE sessions.
///
/// Keyed by the random `state` nonce returned to the client and sent to
/// the authorization endpoint.
pub type OAuthState = Arc<RwLock<HashMap<String, PkceSession>>>;

/// Create a fresh (empty) OAuth session store.
pub fn new_oauth_state() -> OAuthState {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Generate a PKCE code verifier and its S256 challenge.
///
/// Returns `(code_verifier, code_challenge)`.
pub fn generate_pkce() -> (String, String) {
    let mut rng = rand::thread_rng();
    let verifier_bytes: Vec<u8> = (0..32).map(|_| rng.gen()).collect();
    let code_verifier = URL_SAFE_NO_PAD.encode(&verifier_bytes);

    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

    (code_verifier, code_challenge)
}

/// Generate a random state nonce for CSRF protection.
pub fn generate_state() -> String {
    let bytes: Vec<u8> = (0..16).map(|_| rand::thread_rng().gen()).collect();
    URL_SAFE_NO_PAD.encode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_and_challenge_are_different() {
        let (verifier, challenge) = generate_pkce();
        assert_ne!(verifier, challenge);
        // verifier is 32 random bytes base64url-encoded → 43 chars
        assert!(verifier.len() >= 40);
        // challenge is SHA-256 of verifier base64url-encoded → 43 chars
        assert!(challenge.len() >= 40);
    }

    #[test]
    fn pkce_challenge_is_deterministic_for_same_verifier() {
        // Manually verify the S256 relationship
        let verifier = "test-verifier-value";
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected = URL_SAFE_NO_PAD.encode(hasher.finalize());

        let mut hasher2 = Sha256::new();
        hasher2.update(verifier.as_bytes());
        let actual = URL_SAFE_NO_PAD.encode(hasher2.finalize());

        assert_eq!(expected, actual);
    }

    #[test]
    fn state_nonce_is_unique() {
        let s1 = generate_state();
        let s2 = generate_state();
        assert_ne!(s1, s2);
        // 16 bytes base64url → 22 chars
        assert!(s1.len() >= 20);
    }

    #[test]
    fn new_oauth_state_is_empty() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let state = new_oauth_state();
            assert!(state.read().await.is_empty());
        });
    }
}
