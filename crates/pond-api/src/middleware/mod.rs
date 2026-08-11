//! Authentication, rate limiting, onboarding, and debug-logging middleware.

pub mod onboarding_guard;

use axum::{
    extract::{Request, State},
    http::{header, header::HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use pond_core::security::ports::policy::Principal;
use pond_core::user_data::ports::onboarding::OnboardingRepository;
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

/// When a route stops answering a caller with no bearer token.
///
/// P0 made the allowlist method-scoped. This is the second axis, and it exists
/// because the wizard's writes had no reason to stay open one request longer
/// than the wizard: an unauthenticated caller on the LAN could rewrite settings
/// and edit any household member's preferences on a pond that finished setting
/// itself up months ago.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exposure {
    /// Public in every state. Pairing, health, the one question a client must
    /// be able to ask before it has anything to authenticate with, and the
    /// diagnostics that were never justified by onboarding in the first place.
    Always,
    /// Public only while onboarding is incomplete. The wizard's own surface.
    UntilOnboarded,
    /// As [`Exposure::UntilOnboarded`], and once the pond is set up it still
    /// answers a **loopback** caller with no token.
    ///
    /// Exactly one route needs this and the reason is worth stating in full.
    /// `POST /onboard/reset` is the recovery lever for a misconfigured pond,
    /// and getting back out of a reset requires `PUT /settings` +
    /// `POST /profiles` + `POST /onboard/complete` -- all `UntilOnboarded`,
    /// all open again precisely because the reset made the pond
    /// not-onboarded. So reset cannot be shut outright.
    ///
    /// Nor can it stay unconditionally public: an anonymous caller who can
    /// reset the pond can reopen every hole this class exists to close, and
    /// the closure would be decorative.
    ///
    /// Loopback is the resolution, and it is not a new trust boundary. A pond
    /// that has lost every token can only be re-paired from the host already:
    /// `handshake_pairing_code` and `handshake_issue_pairing_code` both refuse
    /// a non-loopback peer inside the handler, and
    /// `is_identity_assertion_proven` records the same rule -- whoever is at
    /// the console already has the box. This adds no requirement that recovery
    /// did not already have.
    UntilOnboardedThenHostOnly,
}

/// Every `(method, path, exposure)` triple reachable with no bearer token.
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
/// **State-scoped, which is the second half (PAI-2 P7).** Every entry also
/// declares *when* it stops being public. `Exposure::Always` is for routes that
/// must answer before a client can authenticate at all, or that were never
/// justified by onboarding; anything the setup wizard needs is
/// `UntilOnboarded`, so it closes the moment the pond is set up.
///
/// Keep this in step with the public router in `routes::api_routes`.
/// `public_router_and_allowlist_agree` fails the build when they drift, and
/// `the_public_route_classification_is_pinned` fails it when an entry changes
/// class without anybody saying so out loud.
const PUBLIC_ROUTES: &[(Method, &str, Exposure)] = &[
    (Method::GET, "/health", Exposure::Always),
    // Pairing handshake (#93): a device has no token until this completes, and
    // recovery cannot depend on being paired. The two pairing-code routes are
    // loopback-gated inside their handlers.
    (Method::POST, "/handshake", Exposure::Always),
    (Method::POST, "/handshake/init", Exposure::Always),
    (Method::POST, "/handshake/verify", Exposure::Always),
    (Method::POST, "/handshake/refresh", Exposure::Always),
    (Method::POST, "/handshake/revoke", Exposure::Always),
    (Method::GET, "/handshake/pairing-code", Exposure::Always),
    (Method::POST, "/handshake/pairing-code", Exposure::Always),
    // Onboarding: all of these run before any device has paired, and none of
    // them has any business running afterwards.
    (Method::POST, "/onboard", Exposure::UntilOnboarded),
    (Method::POST, "/onboard/complete", Exposure::UntilOnboarded),
    // ...except the status probe, which is a read a client must be able to
    // make before it has anything to authenticate with, in order to find out
    // whether it needs the wizard at all.
    (Method::GET, "/onboard/status", Exposure::Always),
    (
        Method::POST,
        "/onboard/step/{name}",
        Exposure::UntilOnboarded,
    ),
    // The recovery lever. See Exposure::UntilOnboardedThenHostOnly -- this is
    // the only entry in that class and the reason the class exists.
    (
        Method::POST,
        "/onboard/reset",
        Exposure::UntilOnboardedThenHostOnly,
    ),
    // Write-only, and only while the wizard is running. GET /settings is NOT
    // here at all: it serialises the whole Settings struct.
    (Method::PUT, "/settings", Exposure::UntilOnboarded),
    // Local Piper; text -> audio, leaks no user data. This LOOKS like an
    // onboarding hole -- the wizard's voice preview is why it is public -- and
    // it is deliberately not classified as one, because two shipped callers
    // speak through it with no Authorization header long after setup:
    // `playTtsSentence` in pond-desktop/src/modes/voice/WebVoiceBackend.ts and
    // `fetch_tts_bytes` in pond-desktop/src-tauri/src/commands/audio_cmd.rs.
    // Closing it would leave the assistant mute on a set-up pond. That those
    // two callers are unauthenticated is a real finding, and the fix is to give
    // them the token -- a client change, not an allowlist change. Until then,
    // narrowing here breaks the product rather than the attack.
    (Method::POST, "/tts", Exposure::Always),
    // The onboarding WakeWord step calibrates before a device has paired. The
    // Settings "Re-calibrate" control goes through `calibrateWakeWord` in
    // PondApiClient.ts, which DOES attach the bearer token when it has one --
    // checked, not assumed, because that is the difference between this entry
    // and the /tts entry above.
    (Method::POST, "/voice/calibrate", Exposure::UntilOnboarded),
    (Method::DELETE, "/voice/calibrate", Exposure::UntilOnboarded),
    // Diagnostics. These are NOT onboarding holes -- none of them is public
    // because the wizard needed it, so P7 leaves them where P0 left them. That
    // they are unauthenticated at all is a separate question this phase did not
    // answer: /transcribe accepts audio and returns text, /test serves a
    // browser page that drives it, /dev/goose reports agent status, and
    // /system/info reports host details.
    (Method::POST, "/transcribe", Exposure::Always),
    (Method::GET, "/system/info", Exposure::Always),
    (Method::GET, "/test", Exposure::Always),
    (Method::POST, "/test/speak", Exposure::Always),
    (Method::GET, "/dev/goose", Exposure::Always),
    // Create and patch during onboarding. GET /profiles (the household roster)
    // and DELETE /profiles/{id} (removing a member) are deliberately absent.
    (Method::POST, "/profiles", Exposure::UntilOnboarded),
    (Method::PATCH, "/profiles/{id}", Exposure::UntilOnboarded),
    // The two exceptions that live in the *protected* router but must stay
    // reachable without a bearer token, each guarded by something else instead:
    // the browser redirect target, which carries the PKCE state nonce...
    (Method::GET, "/oauth/callback", Exposure::Always),
    // ...and the refresh called by extension subprocesses, which is checked
    // against internal_extension_token inside the handler.
    (Method::POST, "/oauth/refresh", Exposure::Always),
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

/// Which exposure class this request falls into, or `None` when the route is
/// not on the allowlist at all.
fn route_exposure(method: &Method, path: &str) -> Option<Exposure> {
    // Non-API paths are the embedded web dashboard's static assets. `serve_web`
    // only ever reads files, so this stays method-agnostic -- and it returns
    // before anything touches the database, which matters: every asset request
    // goes through here.
    if !path.starts_with("/api/") {
        return Some(Exposure::Always);
    }

    let path = path.strip_prefix("/api/v1").unwrap_or(path);

    PUBLIC_ROUTES
        .iter()
        .find(|(m, p, _)| m == method && path_matches(p, path))
        .map(|(_, _, exposure)| *exposure)
}

/// Does this route answer a caller with no token, given the pond's state and
/// where the caller is?
///
/// Pure on purpose. Every input is passed in, so the whole matrix is unit
/// testable and there is nowhere for a cached "this pond has been onboarded
/// before" latch to hide. A latch here would make `POST /onboard/reset` a
/// one-way door: the reset succeeds, the pond drops back to the wizard, and the
/// three routes the wizard needs stay shut forever. The only repair would be
/// reflashing the device. `reset_never_becomes_a_one_way_door` is the guard.
fn public_without_token(exposure: Exposure, onboarded: bool, peer_is_loopback: bool) -> bool {
    match exposure {
        Exposure::Always => true,
        Exposure::UntilOnboarded => !onboarded,
        Exposure::UntilOnboardedThenHostOnly => !onboarded || peer_is_loopback,
    }
}

/// Could this route EVER answer a caller with no token, in some state?
///
/// This is what the compile-time drift guards must ask. They parse `routes.rs`
/// through `include_str!` and run with no database and no pond, so they cannot
/// evaluate "public only while onboarding is incomplete" -- and a guard that
/// picked one state would report the other state's answer as safety. The only
/// question a static check can answer honestly is the worst case.
///
/// `cfg(test)` because the drift guards are its only callers and must stay so:
/// production has a pond to read, and answering a live request from the worst
/// case would re-open every hole this phase closes.
#[cfg(test)]
fn reachable_without_token_in_some_state(method: &Method, path: &str) -> bool {
    route_exposure(method, path).is_some()
}

/// Whether the pond is set up, read LIVE, on every request that needs it.
///
/// Deliberately not cached, not memoised and not a flag on `Settings`. See
/// [`public_without_token`] for why a latch turns reset into a one-way door.
/// `require_onboarding_complete` already reads this state per request, so this
/// is the same cost model, and only the two state-dependent exposure classes
/// pay it -- `Exposure::Always` short-circuits before the call.
///
/// **A read that fails is treated as onboarded, i.e. closed.** On failure,
/// access narrows: the alternative is that a transient `SQLITE_BUSY` re-opens
/// every onboarding write hole on a pond that finished setting up long ago.
/// This is why the port has `is_complete` at all -- `get_current_step` reports
/// an unreadable table as "not started", which is the widest possible answer.
async fn pond_is_onboarded(state: &crate::AppState) -> bool {
    // `--skip-onboarding` means "this pond is not running the wizard". Reading
    // it as "the wizard is still running" would leave the holes open, so it
    // resolves the same way a completed pond does.
    if state.skip_onboarding {
        return true;
    }
    match state.onboarding_repo.is_complete().await {
        Ok(complete) => complete,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not read onboarding state; treating the pond as set up so the \
                 onboarding write holes stay shut"
            );
            true
        }
    }
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
    // Axum stores the peer address as ConnectInfo<SocketAddr> (not bare
    // SocketAddr) when the server is started with
    // into_make_service_with_connect_info, which `main.rs` does. A request that
    // arrived without one is treated as remote -- on failure, access narrows.
    let peer_is_loopback = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().is_loopback())
        .unwrap_or(false);

    if let Some(exposure) = route_exposure(req.method(), path.path()) {
        // Only the state-dependent classes cost a database read. `Always`
        // covers every static asset and /health and must never take one.
        let onboarded = exposure != Exposure::Always && pond_is_onboarded(state.as_ref()).await;
        if public_without_token(exposure, onboarded, peer_is_loopback) {
            let mut req = req;
            // `onboarded` can only be true here for the host-recovery class:
            // every other class that passes does so because the pond is still
            // being set up. So this names the one privileged anonymous caller
            // this middleware grants -- the operator standing at the pond,
            // resetting it -- and leaves the wizard's own requests
            // unattributed, exactly as before.
            if onboarded {
                req.extensions_mut().insert(Principal::loopback());
            }
            return Ok(next.run(req).await);
        }
    }

    // Local-dev convenience: same-device clients (loopback 127.0.0.1 / ::1)
    // MAY skip token validation, but ONLY when the operator explicitly opts in
    // with `POND_DEV_ALLOW_LOOPBACK=1`. This is OFF by default (#94): the old
    // blanket loopback bypass meant any process on the host — not just the
    // desktop app — reached every protected route unauthenticated. With it off,
    // even loopback clients (including the desktop app) must present a valid
    // token obtained via the handshake.
    //
    // PAI-1 P9: this returns EARLY, before a token is read, so the principal it
    // inserts carries no device and the paired-device rung is unreachable on
    // this path. That is deliberate and it is the narrowing answer. There is no
    // token here to look a device up by, and the two tempting substitutes are
    // both wrong in the direction this programme exists to prevent: the most
    // recently paired device would make every local request speak as whoever
    // last paired a phone, and a client-supplied id would outrank every proof
    // the pond can make. A loopback caller falls through to explicit, then
    // face, then guest -- exactly as it did before this rung existed.
    if dev_allow_loopback() && peer_is_loopback {
        let mut req = req;
        req.extensions_mut().insert(Principal::loopback());
        return Ok(next.run(req).await);
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
    // An adapter that cannot name the caller returns None, and the principal
    // says so rather than guessing.
    //
    // -- PAI-1 P9: the device rides here, and only from here -----------------
    //
    // `caller_for_token` is the ONE door the device id comes through. It is the
    // token THIS POND ISSUED at pairing, so the chain
    // device -> devices.profile_id -> member is unbroken and contains nothing
    // the client said about itself. `IdentificationSource::PairedDevice`
    // outranks face and explicit identification, so reading the device from a
    // header, a body field or a query parameter would let a client outrank
    // every proof the pond can make. Nothing in `headers` is consulted below
    // except the bearer token already extracted above, and
    // `device_rung_wiring.rs` asserts that by reading this line's ARGUMENT
    // rather than by checking that some device is set somewhere.
    //
    // A failed lookup NARROWS: no client id, no device, and therefore no
    // paired-device rung. The request is still authenticated -- `validate_token`
    // said so -- it is simply unattributed, which is what every request was
    // before this rung existed.
    //
    // `proven_profile_id` stays None on every path here. The device is the
    // proof; turning it into a member is `resolve_turn_scope`'s job, through
    // `DeviceAttribution`, where a failed read is a distinguishable outcome
    // rather than an `unwrap_or_default`.
    let mut principal = match state.handshake.caller_for_token(&token).await {
        Ok(Some(caller)) => Principal::token(caller.client_id).with_device(caller.device_id),
        _ => Principal::token("unknown".to_string()),
    };
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

    /// The middleware's decision, minus the database read: would this request
    /// answer with no token, on a pond in this state, from this peer?
    ///
    /// Deliberately assembled from the same two functions the middleware calls,
    /// rather than restated -- a test that reimplements the rule proves the
    /// reimplementation. The wiring is proved separately over real HTTP in
    /// `onboarding_integration_test`.
    fn answers_without_token(
        method: &Method,
        path: &str,
        onboarded: bool,
        peer_is_loopback: bool,
    ) -> bool {
        match route_exposure(method, path) {
            Some(exposure) => public_without_token(exposure, onboarded, peer_is_loopback),
            None => false,
        }
    }

    const ONBOARDED: bool = true;
    const SETTING_UP: bool = false;
    const FROM_THE_HOST: bool = true;
    const FROM_THE_LAN: bool = false;

    #[test]
    fn test_is_public_route() {
        // Public in every state: pairing, health, the wizard question a client
        // must be able to ask before it can authenticate, the diagnostics, and
        // the dashboard's static assets. Asserted against the *narrowest*
        // state, so an entry that quietly became state-dependent fails here.
        for (method, path) in [
            (Method::GET, "/api/v1/health"),
            (Method::POST, "/api/v1/handshake"),
            (Method::POST, "/api/v1/handshake/init"),
            (Method::POST, "/api/v1/handshake/verify"),
            (Method::POST, "/api/v1/handshake/refresh"),
            (Method::POST, "/api/v1/handshake/revoke"),
            (Method::GET, "/api/v1/handshake/pairing-code"),
            (Method::GET, "/api/v1/onboard/status"),
            (Method::POST, "/api/v1/transcribe"),
            // Two shipped voice callers speak through /tts with no token after
            // setup; see the table entry. If this line ever becomes false, the
            // assistant goes mute on a set-up pond.
            (Method::POST, "/api/v1/tts"),
            (Method::GET, "/api/v1/oauth/callback"),
            (Method::POST, "/api/v1/oauth/refresh"),
            (Method::GET, "/"),
            (Method::GET, "/index.html"),
            (Method::GET, "/assets/main.js"),
        ] {
            assert!(
                answers_without_token(&method, path, ONBOARDED, FROM_THE_LAN),
                "{method} {path} must answer without a token in every state -- \
                 pairing and recovery cannot depend on being paired"
            );
        }

        // Never public, asserted against the WIDEST state: mid-onboarding, from
        // the host. If a route is refused there it is refused everywhere.
        for (method, path) in [
            (Method::POST, "/api/v1/oauth/authorize"),
            (Method::POST, "/api/v1/chat"),
            (Method::GET, "/api/v1/devices"),
        ] {
            assert!(
                !answers_without_token(&method, path, SETTING_UP, FROM_THE_HOST),
                "{method} {path} must require a token even mid-onboarding, from the host"
            );
        }
    }

    /// The four (method, path) pairs PAI-2 P0 reproduced against a live server,
    /// restated with the state axis P7 added.
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
        // payload. In no state, and not even from the host.
        assert!(!answers_without_token(
            &Method::GET,
            "/api/v1/settings",
            SETTING_UP,
            FROM_THE_HOST
        ));
        // ...while the PUT the wizard needs is open while the wizard is
        // running. Same path, different answer: that pairing is what P0 bought.
        assert!(answers_without_token(
            &Method::PUT,
            "/api/v1/settings",
            SETTING_UP,
            FROM_THE_LAN
        ));

        // GET /profiles listed the whole household.
        assert!(!answers_without_token(
            &Method::GET,
            "/api/v1/profiles",
            SETTING_UP,
            FROM_THE_HOST
        ));
        // POST /profiles creates one during onboarding.
        assert!(answers_without_token(
            &Method::POST,
            "/api/v1/profiles",
            SETTING_UP,
            FROM_THE_LAN
        ));

        // DELETE /profiles/{id} removed a household member and returned 204.
        // It was public because of a `starts_with("/profiles/")` prefix test.
        assert!(!answers_without_token(
            &Method::DELETE,
            "/api/v1/profiles/abc-123",
            SETTING_UP,
            FROM_THE_HOST
        ));
        assert!(!answers_without_token(
            &Method::GET,
            "/api/v1/profiles/abc-123",
            SETTING_UP,
            FROM_THE_HOST
        ));
        // PATCH on the same path is what that prefix test existed for.
        assert!(answers_without_token(
            &Method::PATCH,
            "/api/v1/profiles/abc-123",
            SETTING_UP,
            FROM_THE_LAN
        ));
    }

    /// PAI-2 P7's acceptance test: the wizard's surface stops answering
    /// anonymous callers the moment the pond is set up, and being on the host
    /// does not excuse it. Only `/onboard/reset` gets that excuse.
    #[test]
    fn the_onboarding_write_holes_close_once_the_pond_is_set_up() {
        for (method, path) in [
            (Method::PUT, "/api/v1/settings"),
            (Method::POST, "/api/v1/profiles"),
            (Method::PATCH, "/api/v1/profiles/abc-123"),
            (Method::POST, "/api/v1/onboard"),
            (Method::POST, "/api/v1/onboard/complete"),
            (Method::POST, "/api/v1/onboard/step/Basics"),
            (Method::POST, "/api/v1/voice/calibrate"),
            (Method::DELETE, "/api/v1/voice/calibrate"),
        ] {
            assert!(
                answers_without_token(&method, path, SETTING_UP, FROM_THE_LAN),
                "{method} {path} must be open while the wizard runs, or onboarding \
                 deadlocks on a pond nobody can finish setting up"
            );
            assert!(
                !answers_without_token(&method, path, ONBOARDED, FROM_THE_LAN),
                "{method} {path} must close once the pond is set up"
            );
            assert!(
                !answers_without_token(&method, path, ONBOARDED, FROM_THE_HOST),
                "{method} {path} must close once the pond is set up -- being on the \
                 host is a recovery excuse for /onboard/reset alone"
            );
        }

        // The status probe is not a write and must not close: a client has to
        // be able to ask whether it needs the wizard before it has a token.
        assert!(answers_without_token(
            &Method::GET,
            "/api/v1/onboard/status",
            ONBOARDED,
            FROM_THE_LAN
        ));
    }

    /// The trap this phase is really about.
    ///
    /// Reset is the recovery lever for a misconfigured pond, and it stays
    /// reachable after onboarding. If the closure were a latch rather than a
    /// live read, reset would be the last useful request the pond ever
    /// accepted: it drops the pond back to the wizard, and the wizard needs
    /// three routes a latch would keep shut. The fix would be reflashing.
    #[test]
    fn reset_never_becomes_a_one_way_door() {
        // An anonymous phone on the LAN cannot factory-reset the pond. Without
        // this, reset is a bypass for everything above: reset, then walk in
        // through the holes it reopened.
        assert!(
            !answers_without_token(
                &Method::POST,
                "/api/v1/onboard/reset",
                ONBOARDED,
                FROM_THE_LAN
            ),
            "an unauthenticated LAN caller must not be able to reset the pond -- \
             a reset reopens every hole this phase closes"
        );

        // The operator at the pond can. Same boundary as issuing a pairing
        // code, which is already the only way back into a pond that has lost
        // every token -- so this adds no new requirement to recovery.
        assert!(
            answers_without_token(
                &Method::POST,
                "/api/v1/onboard/reset",
                ONBOARDED,
                FROM_THE_HOST
            ),
            "reset must stay reachable from the host, or a badly misconfigured pond \
             can only be repaired by reflashing it"
        );

        // And once it has run, the pond is not onboarded, so everything the
        // wizard needs is open again -- to anyone, exactly as on a fresh
        // install, because that is what a reset pond is.
        for (method, path) in [
            (Method::PUT, "/api/v1/settings"),
            (Method::POST, "/api/v1/profiles"),
            (Method::PATCH, "/api/v1/profiles/abc-123"),
            (Method::POST, "/api/v1/onboard/step/Basics"),
            (Method::POST, "/api/v1/onboard/complete"),
        ] {
            assert!(
                answers_without_token(&method, path, SETTING_UP, FROM_THE_LAN),
                "{method} {path} must reopen after a reset, or a reset pond can only \
                 be fixed by reflashing it"
            );
        }
    }

    /// The classification, restated as the list it is.
    ///
    /// Which routes are state-dependent is a security decision. It has to show
    /// up as an edit to this test, not as a third tuple field somebody adjusted
    /// while adding a route.
    #[test]
    fn the_public_route_classification_is_pinned() {
        let mut state_dependent: Vec<String> = PUBLIC_ROUTES
            .iter()
            .filter(|(_, _, e)| *e != Exposure::Always)
            .map(|(m, p, e)| format!("{m} {p} = {e:?}"))
            .collect();
        state_dependent.sort();

        let expected = vec![
            "DELETE /voice/calibrate = UntilOnboarded".to_string(),
            "PATCH /profiles/{id} = UntilOnboarded".to_string(),
            "POST /onboard = UntilOnboarded".to_string(),
            "POST /onboard/complete = UntilOnboarded".to_string(),
            "POST /onboard/reset = UntilOnboardedThenHostOnly".to_string(),
            "POST /onboard/step/{name} = UntilOnboarded".to_string(),
            "POST /profiles = UntilOnboarded".to_string(),
            "POST /voice/calibrate = UntilOnboarded".to_string(),
            "PUT /settings = UntilOnboarded".to_string(),
        ];

        assert_eq!(
            state_dependent, expected,
            "the set of state-dependent public routes changed. That is a security \
             decision -- if it is the right one, say so here."
        );
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
            // Not "is it public right now" -- this test has no pond and no
            // onboarding state to read. "In SOME state" is the safe question:
            // a protected route that answers anonymously in any state leaks.
            if !reachable_without_token_in_some_state(method, &full) {
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
        // The union of every exposure class: a route in the public router must
        // be on this table whatever its class, and a table entry with no route
        // is an unreachable exemption whatever its class.
        let allowlist: std::collections::BTreeSet<String> = PUBLIC_ROUTES
            .iter()
            .filter(|(_, p, _)| !p.starts_with("/oauth/"))
            .map(|(m, p, _)| format!("{m} {p}"))
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
        // A protected route is not in the public allowlist, in any state...
        assert!(!answers_without_token(
            &Method::GET,
            "/api/v1/devices",
            SETTING_UP,
            FROM_THE_HOST
        ));
        // ...and with no Authorization header, the bearer extractor rejects,
        // which maps to a 401 — i.e. protected routes require a valid token.
        let headers = HeaderMap::new();
        let err = extract_bearer_token(&headers).expect_err("missing token must error");
        assert_eq!(err.into_response().status(), StatusCode::UNAUTHORIZED);
    }
}
