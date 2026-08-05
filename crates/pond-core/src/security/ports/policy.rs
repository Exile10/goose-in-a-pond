//! Driven Port: Security Policy
//!
//! The single, named seam for authorization at the privacy/security boundary.
//!
//! Today GIAP's privacy boundary is **implicit and diffuse**:
//!
//! - `pond-api`'s `auth_middleware` checks Bearer tokens. It once bypassed
//!   validation entirely for loopback connections; since #94 that bypass is
//!   **off by default** and gated behind the `POND_DEV_ALLOW_LOOPBACK`
//!   environment variable (see `dev_allow_loopback` in
//!   `crates/pond-api/src/middleware/mod.rs`). Authentication is therefore
//!   real, but it is still coarse: a valid token grants everything.
//! - [`SecretRepository`](crate::security::ports::secret::SecretRepository)
//!   guards key material, but is callable by anyone holding the `Arc` — there
//!   is no per-extension scope on which secrets a caller may read.
//! - [`OperationalLogRepository`](crate::security::ports::event_log::OperationalLogRepository)
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
    /// The household member this caller has been **proved** to be.
    ///
    /// PAI-1 section 3.3 calls this "the single value every enforcement
    /// decision in PAI-2 keys on". It is `None` for every caller today, and
    /// that is a fact about the schema rather than an oversight: nothing links
    /// a paired device to a member. `session_tokens`, `push_tokens`,
    /// `pairing_codes` and `handshake_challenges` carry no profile column and
    /// the pairing flow never asks who is pairing.
    ///
    /// **Never populate this from an asserted value.** Filling it from a
    /// request body is what [`is_identity_assertion_proven`] exists to refuse,
    /// and filling it from `settings.primary_profile_id` would attribute every
    /// phone in the house to one person. It stays `None` until capturing a
    /// member at pairing time lands as its own phase.
    pub proven_profile_id: Option<String>,
}

impl Principal {
    /// A loopback caller (no remote address).
    pub fn loopback() -> Self {
        Self {
            kind: PrincipalKind::Loopback,
            remote_addr: None,
            proven_profile_id: None,
        }
    }

    /// An in-process caller with no external origin.
    pub fn internal() -> Self {
        Self {
            kind: PrincipalKind::Internal,
            remote_addr: None,
            proven_profile_id: None,
        }
    }

    /// A remote caller authenticated as `client_id` via a session token.
    pub fn token(client_id: impl Into<String>) -> Self {
        Self {
            kind: PrincipalKind::Token(client_id.into()),
            remote_addr: None,
            proven_profile_id: None,
        }
    }

    /// Attach the originating socket address to this principal.
    pub fn with_remote_addr(mut self, addr: impl Into<String>) -> Self {
        self.remote_addr = Some(addr.into());
        self
    }
}

/// How hard the policy bites. Parsed from `settings.security_policy_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyMode {
    /// No evaluation, no audit entries.
    Off,
    /// Evaluate and record every decision; block none.
    Audit,
    /// Denials bite.
    Enforce,
}

impl PolicyMode {
    /// Parse, defaulting to [`PolicyMode::Audit`] for anything unrecognised.
    ///
    /// Unrecognised means `audit` and never `off`: a typo in a config value
    /// must not silently switch the audit trail off, and it must not silently
    /// start enforcing either. `audit` is the only reading that is wrong in
    /// neither direction.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Self::Off,
            "enforce" => Self::Enforce,
            _ => Self::Audit,
        }
    }

    /// Whether a denial actually blocks the caller in this mode.
    pub fn denies_bite(&self) -> bool {
        matches!(self, Self::Enforce)
    }
}

/// What the policy decided, kept separate from what the caller was allowed to do.
///
/// **`allowed` and `denied_reason` are not redundant, and conflating them would
/// destroy the point of `audit` mode.** In `audit` a denial does not block, so
/// `allowed` is `true` while the rule said no. If the audit trail recorded only
/// the effect, every line would read "permitted" — including for exactly the
/// decisions `enforce` would have blocked — and the log could not answer the one
/// question it exists to answer: what would flipping to `enforce` break?
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    /// Whether the caller proceeds. In `audit` this is `true` even for a denial.
    pub allowed: bool,
    /// `Some(reason)` when the rule said no, whatever the mode did about it.
    pub denied_reason: Option<&'static str>,
    /// The mode the decision was taken under, so a log line is self-describing.
    pub mode: PolicyMode,
}

impl PolicyDecision {
    /// The rule permitted this.
    pub fn permit(mode: PolicyMode) -> Self {
        Self {
            allowed: true,
            denied_reason: None,
            mode,
        }
    }

    /// The rule refused. Whether that blocks depends on the mode.
    pub fn refuse(mode: PolicyMode, reason: &'static str) -> Self {
        Self {
            allowed: !mode.denies_bite(),
            denied_reason: Some(reason),
            mode,
        }
    }

    /// True when the rule refused but the mode let it through anyway — the
    /// telemetry `enforce` is waiting on.
    pub fn would_deny(&self) -> bool {
        self.denied_reason.is_some() && self.allowed
    }

    /// The verdict as it should appear in an audit entry: `allow`, `deny`, or
    /// `would_deny`. This is the field to read when asking what `enforce`
    /// would change; `ok` alone cannot tell you.
    pub fn verdict(&self) -> &'static str {
        match (self.denied_reason.is_some(), self.allowed) {
            (false, _) => "allow",
            (true, true) => "would_deny",
            (true, false) => "deny",
        }
    }
}

/// The one rule where a deny is meaningful today: may this caller assert that a
/// session belongs to a named household member?
///
/// `PUT /sessions/{id}/user` takes a `profile_id` from the request body and
/// binds it at [`Explicit`] strength, after which every turn in that session
/// resolves to that member's scope and their memories are injected. There is no
/// ownership check. Any paired device can declare itself any member and read
/// their data — cross-profile access laundered through the session row rather
/// than through a query parameter.
///
/// **Why this and not a scope x principal-kind matrix.** The eight scopes
/// crossed with the three [`PrincipalKind`]s gives twenty-four cells and every
/// one of them has to be `allow`: each kind legitimately needs each scope for
/// something that exists in the code today, and denying `Internal` anything
/// breaks background work silently rather than returning an error to anybody.
/// A matrix of twenty-four allows is not a security control, it is a table that
/// looks like one. The axis that actually discriminates is whether the caller
/// has *proved* the identity it is claiming.
///
/// **Nothing can prove it yet**, so in `enforce` this denies every explicit
/// identification. That is the correct reading and the reason the default is
/// `audit`: the missing rung is the device-to-member link, which is its own
/// phase. Until it lands, this records who is asserting what, which is exactly
/// the evidence needed before anybody flips the mode.
///
/// [`Explicit`]: crate::user_data::domain::session::IdentificationSource
pub fn is_identity_assertion_proven(principal: &Principal, asserted_profile_id: &str) -> bool {
    match &principal.proven_profile_id {
        // A caller may always assert the member it has been proved to be.
        Some(proven) => proven == asserted_profile_id,
        // Loopback is the pond's own console: whoever is at it already has the
        // box. Refusing here would break "this is Liz" on the device itself
        // while protecting nothing a physical attacker could not reach anyway.
        None => matches!(principal.kind, PrincipalKind::Loopback),
    }
}

/// Reason string for a refused identity assertion. A constant so the audit log
/// is greppable and the same text cannot drift between call sites.
pub const REASON_UNPROVEN_IDENTITY: &str =
    "session identity asserted by a principal that has not proved it";

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

#[cfg(test)]
mod policy_rule_tests {
    use super::*;

    #[test]
    fn mode_parses_and_anything_unrecognised_is_audit() {
        assert_eq!(PolicyMode::parse("off"), PolicyMode::Off);
        assert_eq!(PolicyMode::parse("audit"), PolicyMode::Audit);
        assert_eq!(PolicyMode::parse("enforce"), PolicyMode::Enforce);
        assert_eq!(PolicyMode::parse("  ENFORCE "), PolicyMode::Enforce);
        // A typo must not silently disable the audit trail, and must not
        // silently start enforcing. Audit is wrong in neither direction.
        assert_eq!(PolicyMode::parse("enfroce"), PolicyMode::Audit);
        assert_eq!(PolicyMode::parse(""), PolicyMode::Audit);
    }

    /// The distinction the whole design turns on: in audit mode a refusal does
    /// not block, but it must still be visible as a refusal.
    #[test]
    fn a_refusal_in_audit_mode_proceeds_but_still_reports_would_deny() {
        let d = PolicyDecision::refuse(PolicyMode::Audit, REASON_UNPROVEN_IDENTITY);
        assert!(d.allowed, "audit mode must not block");
        assert!(d.would_deny(), "but the refusal must remain visible");
        assert_eq!(d.verdict(), "would_deny");
        assert_eq!(d.denied_reason, Some(REASON_UNPROVEN_IDENTITY));
    }

    #[test]
    fn the_same_refusal_in_enforce_mode_blocks() {
        let d = PolicyDecision::refuse(PolicyMode::Enforce, REASON_UNPROVEN_IDENTITY);
        assert!(!d.allowed);
        assert!(!d.would_deny(), "it did not merely would-deny, it denied");
        assert_eq!(d.verdict(), "deny");
    }

    #[test]
    fn a_permit_is_a_permit_in_every_mode() {
        for mode in [PolicyMode::Off, PolicyMode::Audit, PolicyMode::Enforce] {
            let d = PolicyDecision::permit(mode);
            assert!(d.allowed);
            assert!(!d.would_deny());
            assert_eq!(d.verdict(), "allow");
        }
    }

    /// `ok` alone cannot distinguish these two, which is why the audit entry
    /// records `verdict` as well. Both are `allowed == true`.
    #[test]
    fn permit_and_would_deny_are_indistinguishable_by_allowed_alone() {
        let permit = PolicyDecision::permit(PolicyMode::Audit);
        let refused = PolicyDecision::refuse(PolicyMode::Audit, REASON_UNPROVEN_IDENTITY);
        assert_eq!(permit.allowed, refused.allowed);
        assert_ne!(permit.verdict(), refused.verdict());
    }

    // ── The identity-assertion rule ────────────────────────────────────────

    #[test]
    fn a_token_principal_cannot_assert_a_member_it_has_not_proved() {
        // The live hole: any paired device claiming to be any member.
        let phone = Principal::token("some-phone");
        assert!(!is_identity_assertion_proven(&phone, "liz-profile-id"));
    }

    #[test]
    fn a_principal_may_assert_the_member_it_has_been_proved_to_be() {
        let mut phone = Principal::token("liz-phone");
        phone.proven_profile_id = Some("liz-profile-id".to_string());
        assert!(is_identity_assertion_proven(&phone, "liz-profile-id"));
        // ...and only that member.
        assert!(!is_identity_assertion_proven(&phone, "jerry-profile-id"));
    }

    /// Whoever is at the pond's own console already has the box. Denying here
    /// would break "this is Liz" on the device itself and protect nothing.
    #[test]
    fn loopback_may_assert_any_member() {
        assert!(is_identity_assertion_proven(
            &Principal::loopback(),
            "anyone"
        ));
    }

    /// Internal callers are background work, not a person claiming to be one.
    /// They get no assertion right, because nothing in-process should be
    /// binding a session to a member on somebody's behalf.
    #[test]
    fn internal_may_not_assert_a_member() {
        assert!(!is_identity_assertion_proven(&Principal::internal(), "liz"));
    }

    /// Every constructor must leave the proved identity empty. Populating it
    /// from an asserted value is the exact mistake this rule exists to catch,
    /// so a default that is anything other than None would defeat it silently.
    #[test]
    fn no_constructor_grants_a_proved_identity() {
        for p in [
            Principal::loopback(),
            Principal::internal(),
            Principal::token("c1"),
            Principal::token("c2").with_remote_addr("10.0.0.2:5000"),
        ] {
            assert_eq!(
                p.proven_profile_id, None,
                "a constructor handed out a proved identity: {p:?}"
            );
        }
    }
}
