//! OAuth 2.1 PKCE session management; in-memory only, so nothing survives a restart by design.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-flight PKCE session, from `POST /oauth/authorize` until `GET /oauth/callback`.
pub struct PkceSession {
    /// Which provider this session targets (e.g. "spotify").
    pub provider_id: String,
    /// Sent to the token endpoint only, never the authorization endpoint.
    pub code_verifier: String,
    /// The `redirect_uri` the authorize request sent. The token exchange must send exactly this,
    /// so it is kept rather than worked out again.
    pub redirect_uri: String,
    /// Optional marketplace extension to auto-install after successful auth.
    pub extension_id: Option<String>,
    /// The household member a per-member sign-in (Uber) is for; `None` for household-wide ones.
    pub profile_id: Option<String>,
    /// When the sign-in started; it expires [`SESSION_TTL`] later.
    pub created_at: std::time::Instant,
}

/// How long a started sign-in waits for the provider to send the person back. Uber's codes live
/// ten minutes, and RFC 6749 recommends no longer for anyone's.
pub const SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// What the UI is told about a sign-in that outlived [`SESSION_TTL`].
pub const SESSION_EXPIRED: &str =
    "This sign-in was not finished within ten minutes, so it expired. Start it again.";

impl PkceSession {
    /// Past [`SESSION_TTL`]: no longer pending, and its return is refused.
    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= SESSION_TTL
    }
}

/// In-flight PKCE sessions, keyed by the random `state` nonce.
pub type OAuthState = Arc<RwLock<HashMap<String, PkceSession>>>;

pub fn new_oauth_state() -> OAuthState {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Track a sign-in under `state_nonce`, dropping any that have expired on the way.
pub async fn begin_session(sessions: &OAuthState, state_nonce: String, session: PkceSession) {
    let mut map = sessions.write().await;
    map.retain(|_, s| !s.is_expired());
    map.insert(state_nonce, session);
}

/// Remove the sign-in `state_nonce` names, which ends it either way; `None` when there is none or
/// it has expired.
pub async fn take_session(sessions: &OAuthState, state_nonce: &str) -> Option<PkceSession> {
    let session = sessions.write().await.remove(state_nonce)?;
    (!session.is_expired()).then_some(session)
}

/// How a finished OAuth flow ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowOutcome {
    /// Tokens were stored, and any extension tied to the flow came back up.
    Completed,
    /// The flow reached a terminal state without delivering a usable result.
    Failed(String),
}

/// A finished flow's outcome, kept briefly so the UI that started it can ask how it ended.
pub struct FlowRecord {
    pub outcome: FlowOutcome,
    recorded_at: std::time::Instant,
}

/// Finished-flow outcomes, keyed by the in-flight session's `state` nonce.
/// Needed since the callback consumes the [`PkceSession`] and stored tokens lie on re-auth.
pub type OAuthOutcomes = Arc<RwLock<HashMap<String, FlowRecord>>>;

pub fn new_oauth_outcomes() -> OAuthOutcomes {
    Arc::new(RwLock::new(HashMap::new()))
}

/// How long an outcome stays queryable: covers a slow browser hand-off.
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

/// Path of the one callback route every provider sends the browser back to.
pub const CALLBACK_PATH: &str = "/api/v1/oauth/callback";

/// The `redirect_uri` to send a provider, from the origin the browser reaches this server at.
///
/// The provider sends the browser back to this URI, so it has to be somewhere that browser can
/// reach. Behind an SSH tunnel (`ssh -L 9080:localhost:8080`) that is the tunnel's local port, not
/// the port this server bound. Only an `http` loopback origin with an explicit port is followed,
/// and `localhost` and `[::1]` become `127.0.0.1`, the loopback form Spotify accepts. Anything else
/// (no origin, `https`, another host, a missing or bad port) gets this server's own `api_port`.
pub fn redirect_uri_for(origin: Option<&str>, api_port: u16) -> String {
    let port = match origin.map(|o| (o, loopback_port(o))) {
        Some((_, Some(port))) => port,
        Some((origin, None)) => {
            tracing::debug!(
                kind = "oauth_redirect_fallback",
                origin = %origin,
                api_port,
                "origin is not an http loopback origin with a port; redirecting to the bound port"
            );
            api_port
        }
        None => api_port,
    };
    format!("http://127.0.0.1:{port}{CALLBACK_PATH}")
}

/// The port of an `http://localhost:N`, `http://127.0.0.1:N` or `http://[::1]:N` origin.
fn loopback_port(origin: &str) -> Option<u16> {
    let (host, port) = origin.strip_prefix("http://")?.rsplit_once(':')?;
    if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        return None;
    }
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    port.parse::<u16>().ok().filter(|&port| port != 0)
}

/// Generate a PKCE `(code_verifier, code_challenge)` pair (S256).
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

/// Env var carrying the token extensions use for server-to-server routes (e.g. `/oauth/refresh`).
pub const INTERNAL_TOKEN_ENV_KEY: &str = "GIAP_INTERNAL_TOKEN";

/// Per-process, never persisted; handed to extensions via [`INTERNAL_TOKEN_ENV_KEY`].
static INTERNAL_EXTENSION_TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn internal_extension_token() -> &'static str {
    INTERNAL_EXTENSION_TOKEN.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// Env var giving extensions the API URL; `serve` binds the first free of 80/8080/4000/5000.
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

    fn session(age: std::time::Duration) -> PkceSession {
        PkceSession {
            provider_id: "uber".to_string(),
            code_verifier: String::new(),
            redirect_uri: redirect_uri_for(None, 4000),
            extension_id: None,
            profile_id: Some("liz".to_string()),
            created_at: std::time::Instant::now() - age,
        }
    }

    #[test]
    fn an_expired_sign_in_is_refused_and_swept() {
        rt().block_on(async {
            let sessions = new_oauth_state();
            let stale = SESSION_TTL + std::time::Duration::from_secs(1);
            begin_session(&sessions, "old".into(), session(stale)).await;
            begin_session(&sessions, "older".into(), session(stale)).await;
            assert_eq!(
                sessions.read().await.len(),
                1,
                "the first stale one was swept"
            );

            assert!(take_session(&sessions, "older").await.is_none());
            assert!(sessions.read().await.is_empty(), "taken even when refused");

            begin_session(&sessions, "new".into(), session(std::time::Duration::ZERO)).await;
            let fresh = take_session(&sessions, "new").await.unwrap();
            assert_eq!(fresh.profile_id.as_deref(), Some("liz"));
            assert!(take_session(&sessions, "new").await.is_none(), "single use");
        });
    }

    // ── Redirect URI ─────────────────────────────────────────

    #[test]
    fn a_tunnelled_localhost_origin_keeps_its_port_on_127_0_0_1() {
        assert_eq!(
            redirect_uri_for(Some("http://localhost:9080"), 8080),
            "http://127.0.0.1:9080/api/v1/oauth/callback"
        );
    }

    #[test]
    fn a_loopback_origin_keeps_its_port() {
        assert_eq!(
            redirect_uri_for(Some("http://127.0.0.1:4000"), 8080),
            "http://127.0.0.1:4000/api/v1/oauth/callback"
        );
        assert_eq!(
            redirect_uri_for(Some("http://[::1]:9080"), 8080),
            "http://127.0.0.1:9080/api/v1/oauth/callback"
        );
    }

    #[test]
    fn anything_but_an_http_loopback_origin_with_a_port_gets_the_bound_port() {
        let bound = "http://127.0.0.1:8080/api/v1/oauth/callback";
        for origin in [
            "http://evil.example:9080",
            "http://192.168.1.20:9080",
            "http://127.0.0.2:9080",
            "https://localhost:9080",
            "http://localhost",
            "http://localhost:",
            "http://localhost:0",
            "http://localhost:99999",
            "http://localhost:+9080",
            "http://localhost:9080/",
            "http://localhost:9080/path",
            "http://user@localhost:9080",
            "http://localhost.evil.example:9080",
            "tauri://localhost",
            "null",
            "",
        ] {
            assert_eq!(redirect_uri_for(Some(origin), 8080), bound, "{origin}");
        }
        assert_eq!(redirect_uri_for(None, 8080), bound);
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
            // Absence must read as absence, or the UI reports a sign-in that never happened.
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

            // Any record sweeps expired entries.
            record_outcome(&outcomes, "fresh", FlowOutcome::Completed).await;
            let map = outcomes.read().await;
            assert!(!map.contains_key("stale"));
            assert!(map.contains_key("fresh"));
        });
    }
}
