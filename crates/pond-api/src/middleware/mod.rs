//! Authentication, rate limiting, onboarding, and debug-logging middleware.

pub mod onboarding_guard;

use axum::{
    extract::{Request, State},
    http::{header, header::HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use pond_core::security::ports::policy::Principal;
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

/// Every `(method, path)` pair reachable with no bearer token.
///
/// **Method-scoped, and that is the entire point.** This table replaced a
/// path-only `matches!` whose arms were already *written* as though they were
/// method-scoped -- `"PUT /settings is public"`, `"POST -- create profile"`,
/// `"PATCH /profiles/:id"` -- while the function only ever saw the path. Every
/// other method on those paths was public too, and `routes::api_routes` merges
/// the public and protected routers into one tree behind this single check, so
/// there was nothing downstream to catch it. Reproduced against a running
/// server with no token (PAI-2 P0): `GET /settings` returned the API keys in
/// plaintext, `GET /profiles` listed the household, and `DELETE /profiles/{id}`
/// deleted a member and returned 204.
///
/// `{brace}` segments are wildcards matching exactly one path segment, so
/// `PATCH /profiles/{id}` no longer implies `DELETE /profiles/{id}` -- which is
/// precisely what the old `path.starts_with("/profiles/")` prefix test did.
///
/// Keep this in step with the public router in `routes::api_routes`.
/// `public_router_and_allowlist_agree` fails the build when they drift.
const PUBLIC_ROUTES: &[(Method, &str)] = &[
    (Method::GET, "/health"),
    // Pairing handshake (#93): a device has no token until this completes.
    (Method::POST, "/handshake"),
    (Method::POST, "/handshake/init"),
    (Method::POST, "/handshake/verify"),
    (Method::POST, "/handshake/refresh"),
    (Method::POST, "/handshake/revoke"),
    (Method::GET, "/handshake/pairing-code"),
    (Method::POST, "/handshake/pairing-code"),
    // Onboarding: all of these run before any device has paired.
    (Method::POST, "/onboard"),
    (Method::POST, "/onboard/complete"),
    (Method::GET, "/onboard/status"),
    (Method::POST, "/onboard/step/{name}"),
    (Method::POST, "/onboard/reset"),
    // Write-only. GET /settings is NOT here: it serialises the whole Settings
    // struct, API keys included.
    (Method::PUT, "/settings"),
    (Method::POST, "/tts"), // local Piper; text -> audio, leaks no user data
    (Method::POST, "/transcribe"),
    (Method::POST, "/voice/calibrate"), // onboarding WakeWord step
    (Method::DELETE, "/voice/calibrate"),
    (Method::GET, "/system/info"),
    (Method::GET, "/test"),
    (Method::POST, "/test/speak"),
    (Method::GET, "/dev/goose"),
    // Create and patch during onboarding. GET /profiles (the household roster)
    // and DELETE /profiles/{id} (removing a member) are deliberately absent.
    (Method::POST, "/profiles"),
    (Method::PATCH, "/profiles/{id}"),
    // The two exceptions that live in the *protected* router but must stay
    // reachable without a bearer token, each guarded by something else instead:
    // the browser redirect target, which carries the PKCE state nonce...
    (Method::GET, "/oauth/callback"),
    // ...and the refresh called by extension subprocesses, which is checked
    // against internal_extension_token inside the handler.
    (Method::POST, "/oauth/refresh"),
];

/// Match a route pattern against a concrete path, `{brace}` segments matching
/// exactly one non-empty segment. Segment-wise, so a pattern never matches a
/// longer path than itself.
fn path_matches(pattern: &str, path: &str) -> bool {
    let mut pat = pattern.split('/');
    let mut act = path.split('/');
    loop {
        match (pat.next(), act.next()) {
            (None, None) => return true,
            (Some(p), Some(a)) => {
                if p.starts_with('{') && p.ends_with('}') {
                    // A wildcard still has to match something: `/profiles/`
                    // must not read as `/profiles/{id}`.
                    if a.is_empty() {
                        return false;
                    }
                } else if p != a {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

fn is_public_route(method: &Method, path: &str) -> bool {
    // Non-API paths are the embedded web dashboard's static assets. `serve_web`
    // only ever reads files, so this stays method-agnostic.
    if !path.starts_with("/api/") {
        return true;
    }

    let path = path.strip_prefix("/api/v1").unwrap_or(path);

    PUBLIC_ROUTES
        .iter()
        .any(|(m, p)| m == method && path_matches(p, path))
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
    if is_public_route(req.method(), path.path()) {
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
            let mut req = req;
            req.extensions_mut().insert(Principal::loopback());
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

    // Name the caller for anything downstream that has to make an authorization
    // decision. Until now `validate_token` answered only yes/no, so a handler
    // could know a request was authenticated and still not know who sent it --
    // which is why no `Principal` was ever constructed in production and
    // `SecurityPolicy::allow` had no call site it could reason from.
    //
    // An adapter that cannot name the client returns None, and the principal
    // says so rather than guessing. `proven_profile_id` stays None on every
    // path here: nothing links a token to a household member yet, and inventing
    // that link is the misattribution this whole workstream exists to prevent.
    let client_id = state
        .handshake
        .client_id_for_token(&token)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    let mut principal = Principal::token(client_id);
    if let Some(ci) = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
    {
        principal = principal.with_remote_addr(ci.0.to_string());
    }

    let mut req = req;
    req.extensions_mut().insert(principal);
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
        assert!(is_public_route(&Method::GET, "/api/v1/health"));
        assert!(is_public_route(&Method::POST, "/api/v1/handshake"));
        assert!(is_public_route(&Method::POST, "/api/v1/handshake/init"));
        assert!(is_public_route(&Method::POST, "/api/v1/handshake/verify"));
        assert!(is_public_route(&Method::POST, "/api/v1/handshake/refresh"));
        assert!(is_public_route(&Method::POST, "/api/v1/handshake/revoke"));
        assert!(is_public_route(
            &Method::GET,
            "/api/v1/handshake/pairing-code"
        ));
        assert!(is_public_route(&Method::POST, "/api/v1/onboard"));
        assert!(is_public_route(&Method::GET, "/api/v1/onboard/status"));
        assert!(is_public_route(&Method::POST, "/api/v1/transcribe"));
        assert!(is_public_route(&Method::POST, "/api/v1/tts"));
        assert!(is_public_route(&Method::GET, "/api/v1/oauth/callback"));
        assert!(is_public_route(&Method::POST, "/api/v1/oauth/refresh"));
        assert!(!is_public_route(&Method::POST, "/api/v1/oauth/authorize"));
        assert!(!is_public_route(&Method::POST, "/api/v1/chat"));
        assert!(!is_public_route(&Method::GET, "/api/v1/devices"));
        // Web dashboard static assets are always public
        assert!(is_public_route(&Method::GET, "/"));
        assert!(is_public_route(&Method::GET, "/index.html"));
        assert!(is_public_route(&Method::GET, "/assets/main.js"));
    }

    /// The four (method, path) pairs PAI-2 P0 reproduced against a live server.
    ///
    /// Each was public only because the allowlist matched on path while its
    /// entries were written as though method-scoped. They are the regression
    /// test for the defect itself, stated as the HTTP requests that leaked.
    #[test]
    fn the_pai2_p0_leaks_are_closed() {
        // GET /settings serialises the whole Settings struct -- it returned
        // api_key_gnews and api_key_finnhub in plaintext, with no token.
        // Those fields no longer exist: PAI-2 P2 moved every credential to the
        // SecretRepository, so grepping for them here finds only this note.
        // The route stays protected regardless -- the struct still carries
        // personal configuration, and P0's defect was the allowlist, not the
        // payload.
        assert!(!is_public_route(&Method::GET, "/api/v1/settings"));
        // ...while the PUT that onboarding needs stays open. This pairing is
        // the entire point of the change: same path, different answer.
        assert!(is_public_route(&Method::PUT, "/api/v1/settings"));

        // GET /profiles listed the whole household.
        assert!(!is_public_route(&Method::GET, "/api/v1/profiles"));
        // POST /profiles creates one during onboarding, and stays open.
        assert!(is_public_route(&Method::POST, "/api/v1/profiles"));

        // DELETE /profiles/{id} removed a household member and returned 204.
        // It was public because of a `starts_with("/profiles/")` prefix test.
        assert!(!is_public_route(
            &Method::DELETE,
            "/api/v1/profiles/abc-123"
        ));
        assert!(!is_public_route(&Method::GET, "/api/v1/profiles/abc-123"));
        // PATCH on the same path is what that prefix test existed for.
        assert!(is_public_route(&Method::PATCH, "/api/v1/profiles/abc-123"));
    }

    #[test]
    fn wildcards_match_one_segment_and_never_an_empty_one() {
        assert!(path_matches("/profiles/{id}", "/profiles/abc"));
        // One segment, not a prefix: the old test let anything below through.
        assert!(!path_matches("/profiles/{id}", "/profiles/abc/secrets"));
        // A trailing slash is not an id.
        assert!(!path_matches("/profiles/{id}", "/profiles/"));
        assert!(!path_matches("/profiles/{id}", "/profiles"));
        assert!(path_matches("/onboard/step/{name}", "/onboard/step/voice"));
        assert!(path_matches("/health", "/health"));
        assert!(!path_matches("/health", "/health/sub"));
    }

    // ── Drift guards ────────────────────────────────────────────────────────
    //
    // `routes.rs` is parsed at COMPILE time. Both tests below therefore fail
    // the build the moment somebody adds a route without deciding whether it
    // is public -- which is the failure mode that produced P0, since
    // `public_routes.merge(protected_routes)` puts every route in one tree
    // behind one check and nothing downstream can catch a wrong answer.

    const ROUTES_RS: &str = include_str!("../routes.rs");

    fn block_between(start: &str, end: &str) -> &'static str {
        let s = ROUTES_RS
            .find(start)
            .unwrap_or_else(|| panic!("routes.rs no longer contains {start:?}"));
        let e = ROUTES_RS
            .find(end)
            .unwrap_or_else(|| panic!("routes.rs no longer contains {end:?}"));
        assert!(s < e, "{start:?} must appear before {end:?} in routes.rs");
        &ROUTES_RS[s..e]
    }

    /// `get(` / `post(` / ... as whole identifiers, so `widget(` is not a `get(`
    /// and a handler called `delete_profile(` is not a `delete(`.
    fn method_tokens(seg: &str) -> Vec<Method> {
        let mut found = Vec::new();
        for (name, method) in [
            ("get", Method::GET),
            ("post", Method::POST),
            ("put", Method::PUT),
            ("patch", Method::PATCH),
            ("delete", Method::DELETE),
        ] {
            let needle = format!("{name}(");
            let mut from = 0;
            while let Some(i) = seg[from..].find(&needle) {
                let at = from + i;
                let boundary = seg[..at]
                    .chars()
                    .next_back()
                    .map(|c| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(true);
                if boundary {
                    found.push(method.clone());
                }
                from = at + needle.len();
            }
        }
        found
    }

    /// Every `.route("path", method(handler))` in a block, as (method, path).
    fn routes_in(block: &str) -> Vec<(Method, String)> {
        let mut out = Vec::new();
        let mut rest = block;
        while let Some(i) = rest.find(".route(") {
            rest = &rest[i + ".route(".len()..];
            let Some(q1) = rest.find('"') else { break };
            let after = &rest[q1 + 1..];
            let Some(q2) = after.find('"') else { break };
            let path = after[..q2].to_string();
            let tail = &after[q2..];
            let seg_end = tail.find(".route(").unwrap_or(tail.len());
            for m in method_tokens(&tail[..seg_end]) {
                out.push((m, path.clone()));
            }
            rest = &tail[seg_end..];
        }
        out
    }

    fn protected_block() -> &'static str {
        block_between(
            "let protected_routes = Router::new()",
            "public_routes.merge(protected_routes)",
        )
    }

    /// PAI-2 P0's acceptance test: every route in the protected router requires
    /// a token.
    ///
    /// The two exceptions are real and each is guarded by something other than
    /// a bearer token. They are listed individually rather than skipped by
    /// prefix so that a third `/oauth/*` route cannot join them silently.
    #[test]
    fn every_protected_route_requires_a_token() {
        let allowed_without_token: &[(Method, &str)] = &[
            // Browser redirect target -- the caller is the user's browser
            // arriving from the provider, which has no token to present. The
            // PKCE state nonce is what authenticates it.
            (Method::GET, "/oauth/callback"),
            // Called by extension subprocesses, checked against
            // internal_extension_token inside the handler.
            (Method::POST, "/oauth/refresh"),
        ];

        let routes = routes_in(protected_block());
        assert!(
            routes.len() > 50,
            "parsed only {} protected routes -- the parser has broken, not the router",
            routes.len()
        );

        let mut leaked = Vec::new();
        for (method, path) in &routes {
            let full = format!("/api/v1{path}");
            if !is_public_route(method, &full) {
                continue;
            }
            let excused = allowed_without_token
                .iter()
                .any(|(m, p)| m == method && p == path);
            if !excused {
                leaked.push(format!("{method} {full}"));
            }
        }
        assert!(
            leaked.is_empty(),
            "these protected routes are reachable with NO TOKEN:\n  {}\n\
             Either add the route to PUBLIC_ROUTES with a reason, or fix the allowlist.",
            leaked.join("\n  ")
        );
    }

    /// The allowlist and the public router must describe the same set.
    ///
    /// Drift in either direction is a defect: a public route missing from the
    /// allowlist 401s during onboarding, and an allowlist entry with no public
    /// route is an unreachable exemption that outlives the reason for it.
    #[test]
    fn public_router_and_allowlist_agree() {
        let router: std::collections::BTreeSet<String> = routes_in(block_between(
            "let public_routes = Router::new()",
            "let protected_routes = Router::new()",
        ))
        .iter()
        .map(|(m, p)| format!("{m} {p}"))
        .collect();

        // The two oauth entries live in the protected router by design; they
        // are covered by every_protected_route_requires_a_token instead.
        let allowlist: std::collections::BTreeSet<String> = PUBLIC_ROUTES
            .iter()
            .filter(|(_, p)| !p.starts_with("/oauth/"))
            .map(|(m, p)| format!("{m} {p}"))
            .collect();

        let missing: Vec<_> = router.difference(&allowlist).collect();
        let extra: Vec<_> = allowlist.difference(&router).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "public router and PUBLIC_ROUTES have drifted.\n\
             in the router, not the allowlist (these will 401 during onboarding): {missing:?}\n\
             in the allowlist, not the router (unreachable exemptions): {extra:?}"
        );
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
        assert!(!is_public_route(&Method::GET, "/api/v1/devices"));
        // ...and with no Authorization header, the bearer extractor rejects,
        // which maps to a 401 — i.e. protected routes require a valid token.
        let headers = HeaderMap::new();
        let err = extract_bearer_token(&headers).expect_err("missing token must error");
        assert_eq!(err.into_response().status(), StatusCode::UNAUTHORIZED);
    }
}
