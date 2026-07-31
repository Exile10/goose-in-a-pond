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

/// How a finished OAuth flow ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowOutcome {
    /// Tokens were stored, and any extension tied to the flow came back up.
    Completed,
    /// The flow reached a terminal state without delivering a usable result.
    Failed(String),
}

/// A finished flow's outcome, retained briefly so the UI that started it can
/// ask how it ended.
pub struct FlowRecord {
    pub outcome: FlowOutcome,
    recorded_at: std::time::Instant,
}

/// Outcomes of finished OAuth flows, keyed by the same `state` nonce the
/// in-flight session used.
///
/// The callback consumes the [`PkceSession`] when it runs, so presence in the
/// session map cannot answer "did my sign-in work?" — an absent nonce is
/// indistinguishable from one that never existed. Without this, the only
/// signal available to the UI is whether the token key exists in the secret
/// store, which is already true whenever the user is *re*-authorising: the
/// poll then reports success ~2 seconds in, regardless of what the user did in
/// the browser.
pub type OAuthOutcomes = Arc<RwLock<HashMap<String, FlowRecord>>>;

/// Create a fresh (empty) OAuth outcome store.
pub fn new_oauth_outcomes() -> OAuthOutcomes {
    Arc::new(RwLock::new(HashMap::new()))
}

/// How long a finished flow's outcome stays queryable. Long enough for a slow
/// browser hand-off, short enough that abandoned flows do not accumulate.
pub const OUTCOME_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Record how a flow ended, evicting anything past [`OUTCOME_TTL`] on the way.
pub async fn record_outcome(outcomes: &OAuthOutcomes, state_nonce: &str, outcome: FlowOutcome) {
    let mut map = outcomes.write().await;
    map.retain(|_, r| r.recorded_at.elapsed() < OUTCOME_TTL);
    map.insert(
        state_nonce.to_string(),
        FlowRecord {
            outcome,
            recorded_at: std::time::Instant::now(),
        },
    );
}

/// Look up how a flow ended, treating an expired record as absent.
pub async fn peek_outcome(outcomes: &OAuthOutcomes, state_nonce: &str) -> Option<FlowOutcome> {
    let map = outcomes.read().await;
    map.get(state_nonce)
        .filter(|r| r.recorded_at.elapsed() < OUTCOME_TTL)
        .map(|r| r.outcome.clone())
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

/// Env var name spawned extension subprocesses read to auth to
/// server-to-server routes (e.g. `/oauth/refresh`) without a handshake token.
pub const INTERNAL_TOKEN_ENV_KEY: &str = "GIAP_INTERNAL_TOKEN";

/// Shared with spawned extension subprocesses via [`INTERNAL_TOKEN_ENV_KEY`].
/// Generated once per process run, never persisted.
static INTERNAL_EXTENSION_TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn internal_extension_token() -> &'static str {
    INTERNAL_EXTENSION_TOKEN.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// Env var naming the local API base URL, read by extension subprocesses that
/// call back into GIAP (e.g. the music extension refreshing its Spotify token).
///
/// Extensions cannot assume a port: `serve` binds the first free one of
/// 80 / 8080 / 4000 / 5000, so the value has to be handed down at spawn time.
pub const GIAP_SERVER_URL_ENV_KEY: &str = "GIAP_SERVER_URL";

/// The loopback base URL for this server, for [`GIAP_SERVER_URL_ENV_KEY`].
pub fn local_server_url(api_port: u16) -> String {
    format!("http://127.0.0.1:{api_port}")
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
    fn internal_extension_token_is_stable_and_nonempty() {
        let t1 = internal_extension_token();
        let t2 = internal_extension_token();
        assert_eq!(t1, t2);
        assert!(!t1.is_empty());
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

    // ── Flow outcomes ────────────────────────────────────────

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    #[test]
    fn an_unstarted_flow_has_no_outcome() {
        rt().block_on(async {
            let outcomes = new_oauth_outcomes();
            // The whole point: absence must be reported as absence. If this
            // ever answered "completed", the UI would report a sign-in that
            // never happened — the bug this store exists to prevent.
            assert_eq!(peek_outcome(&outcomes, "never-issued").await, None);
        });
    }

    #[test]
    fn outcomes_are_recorded_and_read_back_per_nonce() {
        rt().block_on(async {
            let outcomes = new_oauth_outcomes();
            record_outcome(&outcomes, "nonce-a", FlowOutcome::Completed).await;
            record_outcome(
                &outcomes,
                "nonce-b",
                FlowOutcome::Failed("token exchange failed".into()),
            )
            .await;

            assert_eq!(
                peek_outcome(&outcomes, "nonce-a").await,
                Some(FlowOutcome::Completed)
            );
            assert_eq!(
                peek_outcome(&outcomes, "nonce-b").await,
                Some(FlowOutcome::Failed("token exchange failed".into()))
            );
            // One flow's result must never answer for another's.
            assert_eq!(peek_outcome(&outcomes, "nonce-c").await, None);
        });
    }

    #[test]
    fn a_later_flow_supersedes_an_earlier_one_on_the_same_nonce() {
        rt().block_on(async {
            let outcomes = new_oauth_outcomes();
            record_outcome(&outcomes, "n", FlowOutcome::Failed("first".into())).await;
            record_outcome(&outcomes, "n", FlowOutcome::Completed).await;
            assert_eq!(
                peek_outcome(&outcomes, "n").await,
                Some(FlowOutcome::Completed)
            );
        });
    }

    #[test]
    fn expired_outcomes_read_as_absent_and_get_evicted() {
        rt().block_on(async {
            let outcomes = new_oauth_outcomes();
            {
                // Backdate past the TTL without waiting five minutes.
                let mut map = outcomes.write().await;
                map.insert(
                    "stale".to_string(),
                    FlowRecord {
                        outcome: FlowOutcome::Completed,
                        recorded_at: std::time::Instant::now()
                            - OUTCOME_TTL
                            - std::time::Duration::from_secs(1),
                    },
                );
            }

            assert_eq!(peek_outcome(&outcomes, "stale").await, None);

            // Recording anything sweeps expired entries rather than letting
            // abandoned flows accumulate for the life of the process.
            record_outcome(&outcomes, "fresh", FlowOutcome::Completed).await;
            let map = outcomes.read().await;
            assert!(!map.contains_key("stale"));
            assert!(map.contains_key("fresh"));
        });
    }
}
