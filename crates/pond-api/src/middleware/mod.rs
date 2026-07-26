//! Authentication, rate limiting, onboarding, and debug-logging middleware.

pub mod onboarding_guard;

use axum::{
    extract::{Request, State},
    http::{header, header::HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

/// Error type for authentication failures
#[derive(Debug)]
pub enum AuthError {
    MissingToken,
    InvalidFormat,
    InvalidToken,
    /// Carries how long the client should wait, for `Retry-After`.
    RateLimitExceeded {
        retry_after_secs: u64,
    },
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            AuthError::MissingToken => (StatusCode::UNAUTHORIZED, "Missing Authorization header"),
            AuthError::InvalidFormat => (
                StatusCode::UNAUTHORIZED,
                "Invalid Authorization header format. Use: Authorization: Bearer <token>",
            ),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid or expired token"),
            AuthError::RateLimitExceeded { .. } => {
                (StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded")
            }
        };
        let body =
            serde_json::json!({ "error": error_message, "status": status.as_u16() }).to_string();
        match self {
            // RFC 9110: a 429 SHOULD tell the client how long to wait.
            AuthError::RateLimitExceeded { retry_after_secs } => (
                status,
                [(header::RETRY_AFTER, retry_after_secs.to_string())],
                body,
            )
                .into_response(),
            _ => (status, body).into_response(),
        }
    }
}

/// Extracts Bearer token from Authorization header
pub fn extract_bearer_token(headers: &HeaderMap) -> Result<String, AuthError> {
    let auth_header = headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or(AuthError::MissingToken)?;

    if !auth_header.starts_with("Bearer ") {
        return Err(AuthError::InvalidFormat);
    }
    Ok(auth_header[7..].to_string())
}

/// Per-client fixed-window rate limiter.
///
/// Note this is a fixed window, not a token bucket: a client can spend its
/// whole allowance at the end of one window and again at the start of the
/// next, so the true worst-case burst is 2x `max_requests`. That is
/// acceptable here — the limiter exists to stop runaway clients and online
/// guessing, not to shape traffic precisely.
pub struct RateLimiter {
    clients: Arc<RwLock<HashMap<String, ClientRateLimit>>>,
    max_requests: usize,
    window_duration: Duration,
    last_cleanup: Arc<RwLock<Instant>>,
}

#[derive(Clone)]
struct ClientRateLimit {
    window_start: Instant,
    request_count: usize,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window_duration: Duration) -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            max_requests,
            window_duration,
            last_cleanup: Arc::new(RwLock::new(Instant::now())),
        }
    }

    pub async fn check_rate_limit(&self, client_id: &str) -> bool {
        self.check_rate_limit_detailed(client_id).await.is_ok()
    }

    /// As [`Self::check_rate_limit`], but on rejection reports how long the
    /// caller should wait before retrying, for the `Retry-After` header.
    /// Without that signal a client has nothing to base a backoff on, and a
    /// naive one retries as fast as it can — which is exactly how a single
    /// wedged client burns the whole per-IP budget.
    pub async fn check_rate_limit_detailed(&self, client_id: &str) -> Result<(), Duration> {
        let mut clients = self.clients.write().await;
        let now = Instant::now();
        let client = clients
            .entry(client_id.to_string())
            .or_insert(ClientRateLimit {
                window_start: now,
                request_count: 0,
            });
        if now.duration_since(client.window_start) > self.window_duration {
            client.window_start = now;
            client.request_count = 0;
        }
        let allowed = if client.request_count < self.max_requests {
            client.request_count += 1;
            Ok(())
        } else {
            // Whatever is left of the current window.
            Err(self
                .window_duration
                .saturating_sub(now.duration_since(client.window_start)))
        };

        // Periodic eviction of stale entries to prevent unbounded growth.
        let mut lc = self.last_cleanup.write().await;
        if now.duration_since(*lc) > self.window_duration * 2 {
            clients.retain(|_, v| now.duration_since(v.window_start) <= self.window_duration);
            *lc = now;
        }

        allowed
    }
}

/// `Retry-After` is expressed in whole seconds, and a value of 0 would invite
/// an immediate retry — round any remaining wait up to at least 1.
pub fn retry_after_secs(remaining: Duration) -> u64 {
    remaining.as_secs().max(1)
}

/// Routes that don't require authentication.
///
/// Must stay in sync with the public route set in `routes::api_routes()`.
/// Whether to allow unauthenticated loopback clients (local-dev escape hatch).
/// Off unless `POND_DEV_ALLOW_LOOPBACK` is set to a truthy value.
fn dev_allow_loopback() -> bool {
    loopback_flag_enabled(std::env::var("POND_DEV_ALLOW_LOOPBACK").ok().as_deref())
}

/// Pure truthiness check for the loopback escape-hatch flag. Separated from the
/// env read so it can be unit-tested without mutating shared process env.
fn loopback_flag_enabled(value: Option<&str>) -> bool {
    matches!(value, Some("1") | Some("true") | Some("TRUE"))
}

fn is_public_route(path: &str) -> bool {
    // Non-API paths are static web assets — always public
    if !path.starts_with("/api/") {
        return true;
    }

    let path = path.strip_prefix("/api/v1").unwrap_or(path);

    // Exact public matches
    matches!(
        path,
        "/health"
            | "/handshake"
            | "/handshake/init"
            | "/handshake/verify"
            | "/handshake/refresh"
            | "/handshake/revoke"
            | "/handshake/pairing-code"
            | "/onboard"
            | "/onboard/complete"
            | "/onboard/status"
            | "/transcribe"
            | "/tts"              // public — local Piper, no user data leaked
            | "/voice/calibrate"   // POST/DELETE — used during onboarding WakeWord step
            | "/system/info"
            | "/test"
            | "/test/speak"
            | "/dev/goose"
            | "/profiles"         // POST — create profile during onboarding
            | "/oauth/callback"   // browser redirect target; protected by PKCE state nonce instead
            | "/oauth/refresh"    // called by extension subprocesses; checked against internal_extension_token
    )
    // PUT /settings is public so onboarding steps can save before completion
    || path == "/settings"
    // PATCH /profiles/:id — update profile preferences during onboarding
    || (path.starts_with("/profiles/") && !path.ends_with("/profiles/"))
}

/// Debug request/response logging middleware.
///
/// Emits one `DEBUG` span for each incoming request and one for the outgoing
/// response. Because both use [`tracing::debug!`], they are only visible when
/// the active log filter includes the `debug` level — i.e. when the server is
/// started with `pond-server serve --debug`. At the default `info` level this
/// middleware is a zero-cost pass-through; no branching or allocation occurs.
///
/// Logged fields:
/// - `-->` line: HTTP method, path, and query string (empty string when absent)
/// - `<--` line: HTTP method, path, response status code, elapsed time in ms
pub async fn log_requests(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = std::time::Instant::now();

    tracing::debug!(
        method = %method,
        path   = %uri.path(),
        query  = %uri.query().unwrap_or(""),
        "--> incoming request"
    );

    let response = next.run(req).await;

    tracing::debug!(
        method     = %method,
        path       = %uri.path(),
        status     = response.status().as_u16(),
        latency_ms = start.elapsed().as_millis(),
        "<-- outgoing response"
    );

    response
}

/// Global token-based authentication middleware.
///
/// Uses `from_fn_with_state` so it can access `AppState::handshake` to
/// validate the Bearer token against the in-memory token store.
/// Public routes (health, handshake, onboarding, static assets) bypass
/// token validation entirely.
pub async fn auth_middleware(
    State(state): State<Arc<crate::AppState>>,
    headers: axum::http::HeaderMap,
    path: axum::http::Uri,
    req: Request,
    next: Next,
) -> Result<Response, AuthError> {
    if is_public_route(path.path()) {
        return Ok(next.run(req).await);
    }

    // Local-dev convenience: same-device clients (loopback 127.0.0.1 / ::1)
    // MAY skip token validation, but ONLY when the operator explicitly opts in
    // with `POND_DEV_ALLOW_LOOPBACK=1`. This is OFF by default (#94): the old
    // blanket loopback bypass meant any process on the host — not just the
    // desktop app — reached every protected route unauthenticated. With it off,
    // even loopback clients (including the desktop app) must present a valid
    // token obtained via the handshake.
    //
    // Axum stores the peer address as ConnectInfo<SocketAddr> (not bare SocketAddr)
    // when the server is started with into_make_service_with_connect_info.
    if dev_allow_loopback() {
        let is_loopback = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip().is_loopback())
            .unwrap_or(false);
        if is_loopback {
            return Ok(next.run(req).await);
        }
    }

    let token = extract_bearer_token(&headers)?;

    let valid = state
        .handshake
        .validate_token(&token)
        .await
        .map_err(|_| AuthError::InvalidToken)?;

    if !valid {
        return Err(AuthError::InvalidToken);
    }

    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_bearer_token_success() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer my-token-123".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers).unwrap(), "my-token-123");
    }

    #[test]
    fn test_extract_bearer_token_missing() {
        assert!(matches!(
            extract_bearer_token(&HeaderMap::new()),
            Err(AuthError::MissingToken)
        ));
    }

    #[test]
    fn test_extract_bearer_token_invalid_format() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Basic my-token-123".parse().unwrap());
        assert!(matches!(
            extract_bearer_token(&headers),
            Err(AuthError::InvalidFormat)
        ));
    }

    #[tokio::test]
    async fn test_rate_limiter_allows_requests_within_limit() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        for _ in 0..5 {
            assert!(limiter.check_rate_limit("client-1").await);
        }
        assert!(!limiter.check_rate_limit("client-1").await);
    }

    /// A rejected caller learns how long to wait, so it can back off instead
    /// of hot-looping and holding its own budget at zero.
    #[tokio::test]
    async fn rejection_reports_the_remaining_window() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        assert!(limiter.check_rate_limit_detailed("client-1").await.is_ok());

        let remaining = limiter
            .check_rate_limit_detailed("client-1")
            .await
            .expect_err("second request is over the limit");
        assert!(
            remaining <= Duration::from_secs(60) && remaining > Duration::from_secs(55),
            "expected roughly the full window back, got {remaining:?}"
        );
    }

    /// `Retry-After` is in whole seconds and must never say "retry now".
    #[test]
    fn retry_after_never_rounds_down_to_zero() {
        assert_eq!(retry_after_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_after_secs(Duration::ZERO), 1);
        assert_eq!(retry_after_secs(Duration::from_secs(42)), 42);
    }

    /// The 429 body is unchanged, but the header is what a client actually
    /// needs to back off correctly.
    #[test]
    fn rate_limited_response_carries_retry_after() {
        let response = AuthError::RateLimitExceeded {
            retry_after_secs: 17,
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "17");
    }

    /// Only the 429 carries it — a 401 must not imply "wait and retry".
    #[test]
    fn auth_failures_carry_no_retry_after() {
        let response = AuthError::InvalidToken.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(header::RETRY_AFTER).is_none());
    }

    #[tokio::test]
    async fn test_rate_limiter_different_clients() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        assert!(limiter.check_rate_limit("client-1").await);
        assert!(limiter.check_rate_limit("client-1").await);
        assert!(limiter.check_rate_limit("client-2").await);
        assert!(limiter.check_rate_limit("client-2").await);
    }

    #[tokio::test]
    async fn test_rate_limiter_evicts_stale_entries() {
        // Window = 1s, cleanup triggers after 2× window = 2s
        let limiter = RateLimiter::new(100, Duration::from_secs(1));
        limiter.check_rate_limit("stale-client").await;

        // Wait for the entry to go stale and cleanup to trigger
        tokio::time::sleep(Duration::from_secs(3)).await;

        // This call triggers cleanup (>2× window since last cleanup)
        limiter.check_rate_limit("new-client").await;

        let clients = limiter.clients.read().await;
        assert!(
            !clients.contains_key("stale-client"),
            "stale entry should have been evicted"
        );
        assert_eq!(clients.len(), 1, "only the fresh entry should remain");
    }

    #[test]
    fn test_is_public_route() {
        assert!(is_public_route("/api/v1/health"));
        assert!(is_public_route("/api/v1/handshake"));
        assert!(is_public_route("/api/v1/handshake/init"));
        assert!(is_public_route("/api/v1/handshake/verify"));
        assert!(is_public_route("/api/v1/handshake/refresh"));
        assert!(is_public_route("/api/v1/handshake/revoke"));
        assert!(is_public_route("/api/v1/handshake/pairing-code"));
        assert!(is_public_route("/api/v1/onboard"));
        assert!(is_public_route("/api/v1/onboard/status"));
        assert!(is_public_route("/api/v1/transcribe"));
        assert!(is_public_route("/api/v1/tts"));
        assert!(is_public_route("/api/v1/oauth/callback"));
        assert!(is_public_route("/api/v1/oauth/refresh"));
        assert!(!is_public_route("/api/v1/oauth/authorize"));
        assert!(!is_public_route("/api/v1/chat"));
        assert!(!is_public_route("/api/v1/devices"));
        // PUT /settings is public so onboarding wizard steps can save before handshake completes
        assert!(is_public_route("/api/v1/settings"));
        // Web dashboard static assets are always public
        assert!(is_public_route("/"));
        assert!(is_public_route("/index.html"));
        assert!(is_public_route("/assets/main.js"));
    }

    #[test]
    fn test_auth_error_to_response() {
        let err = AuthError::MissingToken;
        assert_eq!(err.into_response().status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_loopback_bypass_is_gated_and_off_by_default() {
        // Off unless explicitly enabled — the default (env unset) must be false.
        assert!(!loopback_flag_enabled(None));
        assert!(!loopback_flag_enabled(Some("")));
        assert!(!loopback_flag_enabled(Some("0")));
        assert!(!loopback_flag_enabled(Some("yes")));
        // Only explicit truthy values enable it.
        assert!(loopback_flag_enabled(Some("1")));
        assert!(loopback_flag_enabled(Some("true")));
        assert!(loopback_flag_enabled(Some("TRUE")));
    }

    #[test]
    fn test_protected_route_without_token_is_unauthorized() {
        // A protected route is not in the public allowlist...
        assert!(!is_public_route("/api/v1/devices"));
        // ...and with no Authorization header, the bearer extractor rejects,
        // which maps to a 401 — i.e. protected routes require a valid token.
        let headers = HeaderMap::new();
        let err = extract_bearer_token(&headers).expect_err("missing token must error");
        assert_eq!(err.into_response().status(), StatusCode::UNAUTHORIZED);
    }
}
