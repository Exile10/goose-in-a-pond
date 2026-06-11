//! Driven Port: Security Policy
//!
//! The single, named seam for authorization at the privacy/security boundary.
//!
//! Today GIAP's privacy boundary is **implicit and diffuse**:
//!
//! - `pond-api`'s `auth_middleware` checks Bearer tokens but **bypasses
//!   validation entirely for loopback** connections (see the loopback short-
//!   circuit near the top of `auth_middleware` in
//!   `crates/pond-api/src/middleware/mod.rs`, around lines 206-214). On a
//!   single-device deployment everything on `127.0.0.1` is trusted.
//! - [`SecretRepository`](crate::security::ports::secret::SecretRepository)
//!   guards key material, but is callable by anyone holding the `Arc` — there
//!   is no per-extension scope on which secrets a caller may read.
//! - [`EventLogRepository`](crate::security::ports::event_log::EventLogRepository)
//!   is a generic table with no required call site, so cross-boundary access
//!   currently leaves no consistent audit trail.
//! - [`notification`](crate::mcp::ports::notification) has no consent gate.
//!
//! This port turns that mental model into source. It is a **hook, not a gate**:
//! the default implementation
//! ([`AllowAllPolicy`](crate::security::services::policy::AllowAllPolicy))
//! permits everything and only records audit entries, so wiring it in changes
//! no behaviour. Adoption is route-by-route — a handler opts in by calling
//! [`SecurityPolicy::allow`] before touching a user-data scope, and
//! [`SecurityPolicy::audit`] at the crossing. Real authorization
//! (per-profile memory access, per-extension secret scopes, notification
//! consent) lands here later without re-architecting the request path.

use anyhow::Result;
use async_trait::async_trait;

/// Coarse-grained user-data scope identifiers used by [`SecurityPolicy::allow`].
///
/// Call sites reference these constants instead of scattering string literals
/// so the set of recognised scopes stays in one place. The eight scopes mirror
/// the user-data quadrant's sensitive surfaces.
pub mod scopes {
    /// Stored conversational memory fragments.
    pub const MEMORY: &str = "memory";
    /// Assistant settings (identity, LLM, voice, retention).
    pub const SETTINGS: &str = "settings";
    /// Secret material (API keys, OAuth tokens).
    pub const SECRETS: &str = "secrets";
    /// Household member profiles.
    pub const PROFILE: &str = "profile";
    /// Conversation sessions and their history.
    pub const SESSION: &str = "session";
    /// Scheduled / cron tasks.
    pub const SCHEDULE: &str = "schedule";
    /// IoT sensor readings.
    pub const SENSOR: &str = "sensor";
    /// Camera frames and events.
    pub const CAMERA: &str = "camera";
}

/// Origin of a request crossing the privacy/security boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipalKind {
    /// A request from the loopback interface (`127.0.0.1`). Trusted on a
    /// single-device deployment; the source of today's implicit bypass.
    Loopback,
    /// A remote client authenticated with a session token. The `String` is the
    /// `client_id` the token was issued to (see [`crate::security::ports::handshake`]).
    Token(String),
    /// An in-process caller (background task, scheduler, memory extractor) with
    /// no external origin. Not subject to client-facing auth.
    Internal,
}

/// The authenticated (or implicitly trusted) caller behind a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// How this caller was identified.
    pub kind: PrincipalKind,
    /// Remote socket address, when the request came over the network.
    /// `None` for [`PrincipalKind::Internal`].
    pub remote_addr: Option<String>,
}

impl Principal {
    /// A loopback caller (no remote address).
    pub fn loopback() -> Self {
        Self {
            kind: PrincipalKind::Loopback,
            remote_addr: None,
        }
    }

    /// An in-process caller with no external origin.
    pub fn internal() -> Self {
        Self {
            kind: PrincipalKind::Internal,
            remote_addr: None,
        }
    }

    /// A remote caller authenticated as `client_id` via a session token.
    pub fn token(client_id: impl Into<String>) -> Self {
        Self {
            kind: PrincipalKind::Token(client_id.into()),
            remote_addr: None,
        }
    }

    /// Attach the originating socket address to this principal.
    pub fn with_remote_addr(mut self, addr: impl Into<String>) -> Self {
        self.remote_addr = Some(addr.into());
        self
    }
}

/// Driven Port: authorization decisions and audit at the privacy boundary.
///
/// See the [module docs](self) for why this port exists and how it is adopted.
#[async_trait]
pub trait SecurityPolicy: Send + Sync {
    /// Is this caller allowed to access the named user-data scope?
    ///
    /// Scopes are coarse-grained — use the [`scopes`] constants. The default
    /// implementation returns `Ok(true)` for everything; real rules land here
    /// later. An `Err` signals the policy could not be evaluated (not a denial),
    /// and callers should treat it as fail-closed.
    async fn allow(&self, principal: &Principal, scope: &str) -> Result<bool>;

    /// Append an audit entry. Intended to be called at cross-boundary calls,
    /// recording who did what to which scope and whether it was permitted.
    ///
    /// Auditing must never fail the caller, so this returns nothing — the
    /// implementation swallows or logs its own errors.
    async fn audit(&self, principal: &Principal, action: &str, scope: &str, ok: bool);
}
