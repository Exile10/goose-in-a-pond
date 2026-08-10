//! Proactive proposal domain — PAI-7 P3.
//!
//! A proactive impulse does not perform an action. It produces a **proposal**:
//! a staged action, addressed to one household member, carrying the reason GIAP
//! thinks it matters, that the member approves or rejects. That is PAI-7
//! invariant 1 ("GIAP proposes; the user disposes") and it is the whole safety
//! argument of the workstream.
//!
//! **Policy only.** Nothing here writes a row, reads the bus, or decides when a
//! proposal should be made. Persistence is
//! [`ProposalRepository`](crate::user_data::ports::proposal::ProposalRepository),
//! whose adapter puts a proposal in the `drafts` table so proposals inherit the
//! approval flow `giap-draft` already has rather than growing a second one.
//!
//! # What this module tries to make the compiler enforce
//!
//! Three of PAI-7's seven invariants are about a proposal's shape, and all three
//! are the kind that get written as a runtime check and deleted by a later
//! refactor:
//!
//! - **Invariant 2, every proposal carries a rationale.** A `String` field is
//!   not a mandatory rationale, because `String::new()` is always available.
//!   [`Proposal`]'s fields are private, [`Proposal::from_parts`] refuses a blank
//!   one, and — this is the part that makes it unforgeable rather than merely
//!   validated — `Proposal` derives no `Deserialize`, so there is no serde door
//!   past the constructor. The persistence layer reassembles a stored proposal
//!   through `from_parts` like everybody else, so a row whose rationale was
//!   emptied out of band does not load at all. Migration 0041 adds the third
//!   layer: SQLite refuses to write one.
//!
//! - **Invariant 4, proposals are addressed to a profile, never broadcast.**
//!   Note what that rules out: not only [`ProfileScope::Guest`] but
//!   [`ProfileScope::Household`], which *is* the broadcast. So the audience is
//!   not a `ProfileScope` at all — it is [`ProposalAudience`], which holds a
//!   profile id and nothing else, in a one-type module so its field cannot be
//!   filled from outside. `Household` and `Guest` are unrepresentable rather
//!   than refused. See [`ProposalAudience`] for why that is worth a module.
//!
//! - **Invariant 7, a proposal expires.** An `expires_at` that only ever gets
//!   written is a lie. Three things read it: [`Proposal::is_live_at`], the
//!   repository's read path (which filters in SQL, so an expired proposal
//!   cannot be listed even if no sweeper ever runs), and a trigger in migration
//!   0041 that refuses the `approved` transition. And an expiry with no ceiling
//!   is barely an expiry, so [`MAX_PROPOSAL_TTL`] caps it: "expires in the year
//!   3000" satisfies the field and fails the invariant.
//!
//! # Why `trigger` is not a `BusEvent`
//!
//! [`BusEventRef`] names the event that prompted the proposal without importing
//! the bus's variant list. That is deliberate: `BusEvent` is a closed enum that
//! PAI-7 P1 and P2 are widening with `Time`, `Presence`, `Session` and
//! `Ingest`, and a type here that matched on today's variants would need
//! editing for each one — an assertion window too narrow for the natural
//! change. `kind` is the string the bus's own
//! `#[serde(rename_all = "snake_case", tag = "kind")]` already emits for the
//! variant, so a producer hands over `"camera"` or `"presence"` and this module
//! never learns the difference.

use crate::user_data::domain::schedule::TaskKind;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The `drafts.kind` tag a proposal is stored under.
///
/// User-staged drafts carry a tag the model chose (`"shell_command"`,
/// `"file_write"`); this one is reserved, so a listing can tell a proactive
/// suggestion from something the user asked for in the same breath.
pub const PROPOSAL_DRAFT_KIND: &str = "proposal";

/// The `drafts.origin` value that marks a row as proactive.
///
/// `NULL` means user-staged, which is every row that existed before migration
/// 0041 and every row `save_draft` writes. The column is the read path's filter
/// and the migration's trigger predicate, so this constant is load-bearing in
/// SQL as well as in Rust.
pub const PROPOSAL_ORIGIN: &str = "proactive";

/// The `drafts.session_id` a proposal is stored under.
///
/// `drafts.session_id` is `NOT NULL` (migration 0018) and means "the engine
/// session that staged this action". A proposal has no such session: it comes
/// from a background reviewer, not from a turn. A sentinel is the honest answer,
/// and it is namespaced so it cannot collide with an engine session id — which
/// matters, because `list_drafts` scopes by session and a collision would put a
/// proposal in a stranger's draft list.
pub const PROPOSAL_SESSION_ID: &str = "giap:proactive";

/// The longest a proposal may stay live.
///
/// Invariant 7 is "a proposal expires. An assistant that surfaces yesterday's
/// suggestion has failed twice." A field named `expires_at` satisfies that
/// sentence with a timestamp in the year 3000, so the ceiling is enforced at
/// construction. A day is already generous for "the delivery window closes at
/// six"; anything a reviewer wants to say tomorrow it can propose tomorrow,
/// with tomorrow's evidence.
pub const MAX_PROPOSAL_TTL: Duration = Duration::hours(24);

// ── The trigger reference ───────────────────────────────────────────────────

/// Which event prompted a proposal, in terms that survive the bus growing new
/// variants.
///
/// `kind` is the bus event's serde tag (`"sensor"`, `"camera"`, `"device"`, and
/// whatever PAI-7 P1 and P2 add). `source_id` is the device / camera / profile
/// the event came from and `signal` is the sensor type, camera event type or
/// state key — the same projection `BusEvent::trigger_view` makes for rule
/// evaluation, minus the numeric value, because a proposal quotes its evidence
/// in `rationale` rather than re-deriving it.
///
/// Both are `Option` because not every event family has them, and neither is
/// load-bearing: `kind` and `observed_at` are what PAI-7 P7's feedback loop
/// needs to learn "we never want to be told about the garage door during the
/// day".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "BusEventRefWire")]
pub struct BusEventRef {
    kind: String,
    source_id: Option<String>,
    signal: Option<String>,
    observed_at: DateTime<Utc>,
}

/// The deserialization door onto [`BusEventRef`], so a stored payload goes
/// through [`BusEventRef::new`] rather than around it.
#[derive(Deserialize)]
struct BusEventRefWire {
    kind: String,
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    signal: Option<String>,
    observed_at: DateTime<Utc>,
}

impl TryFrom<BusEventRefWire> for BusEventRef {
    type Error = ProposalError;

    fn try_from(w: BusEventRefWire) -> Result<Self, Self::Error> {
        BusEventRef::new(w.kind, w.source_id, w.signal, w.observed_at)
    }
}

impl BusEventRef {
    /// Build a reference to the event that triggered a proposal.
    ///
    /// Refuses a blank `kind`: a proposal whose trigger is unattributable
    /// cannot be reasoned about by the feedback loop and cannot be explained to
    /// the user, and "" is exactly what a defaulted field produces.
    pub fn new(
        kind: impl Into<String>,
        source_id: Option<String>,
        signal: Option<String>,
        observed_at: DateTime<Utc>,
    ) -> Result<Self, ProposalError> {
        let kind = kind.into();
        if kind.trim().is_empty() {
            return Err(ProposalError::MissingTriggerKind);
        }
        Ok(Self {
            kind: kind.trim().to_string(),
            source_id: blank_to_none(source_id),
            signal: blank_to_none(signal),
            observed_at,
        })
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn source_id(&self) -> Option<&str> {
        self.source_id.as_deref()
    }

    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }

    pub fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }
}

fn blank_to_none(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ── The audience ────────────────────────────────────────────────────────────

/// A one-type module, so that "never broadcast" is a shape rather than a check.
///
/// Rust privacy is per-MODULE, not per-type: a `struct ProposalAudience(String)`
/// declared beside [`Proposal`] has a field `Proposal::from_parts` can fill
/// directly, and the newtype becomes a comment with a type signature. That is
/// the mistake PAI-6 P1 made with `ChildScope` and fixed by moving it into its
/// own module; this is the same move for the same reason.
mod audience {
    use super::ProposalError;
    use crate::user_data::domain::profile::ProfileScope;
    use serde::{Deserialize, Serialize};

    /// The household member a proposal is addressed to.
    ///
    /// # Why this is not a `ProfileScope`
    ///
    /// PAI-7's section 3.2 sketches the field as `profile_scope: ProfileScope`,
    /// and taken with invariants 4 and 5 that type admits exactly one of its
    /// three shapes:
    ///
    /// - [`ProfileScope::Guest`] is refused by invariant 5 — "`Guest` sessions
    ///   generate no proposals and receive none".
    /// - [`ProfileScope::Household`] is refused by invariant 4 — "proposals are
    ///   addressed to a profile, **never broadcast to the household**".
    ///   `Household` is not a weaker address than `Owner`; it *is* the
    ///   broadcast, and it is the one a defaulted field lands on
    ///   ([`ProfileScope::household`] exists precisely as the serde default).
    /// - [`ProfileScope::Owner`] is the only admissible shape.
    ///
    /// A type with one admissible shape out of three should not be that type.
    /// Holding the profile id directly makes both refusals structural: there is
    /// no `ProposalAudience` value that means "everyone", so no later refactor
    /// can produce one, and no `if scope == Household { ... }` can be deleted
    /// because there is none to delete.
    ///
    /// **Chosen over a constructor refusal deliberately.** A refusal is a
    /// branch, and a branch has a call site; PAI-6 P1 recorded the mutation
    /// where deleting the clamp's one call site left all 899 pond-core tests
    /// green, because every guard called the clamp directly and nothing
    /// observed it through the production path. There is no line to delete
    /// here. [`from_scope`](Self::from_scope) is still fallible, because the
    /// producer will hold a `ProfileScope` and something has to be the door —
    /// but the door narrows on failure (no proposal at all) and a caller that
    /// skips it still cannot construct a broadcast.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(try_from = "String", into = "String")]
    pub struct ProposalAudience(String);

    impl ProposalAudience {
        /// Address a proposal to a named household member.
        pub fn for_member(profile_id: impl Into<String>) -> Result<Self, ProposalError> {
            let id = profile_id.into();
            let id = id.trim();
            if id.is_empty() {
                return Err(ProposalError::UnaddressableAudience {
                    scope: "an empty profile id".to_string(),
                });
            }
            Ok(Self(id.to_string()))
        }

        /// The one door from the scope lattice, for a producer holding the
        /// scope a turn resolved.
        ///
        /// `Household` and `Guest` are refused for the reasons in the type
        /// docs, and the refusal narrows: a reviewer that cannot name a member
        /// emits no proposal, rather than one everybody can see. On a pond with
        /// no profile rows that means no proactive suggestions at all, which is
        /// the correct answer to "who is this for?" when nobody is on file.
        pub fn from_scope(scope: &ProfileScope) -> Result<Self, ProposalError> {
            match scope.owner_id() {
                Some(id) => Self::for_member(id),
                None => Err(ProposalError::UnaddressableAudience {
                    scope: match scope {
                        ProfileScope::Household => "the whole household".to_string(),
                        ProfileScope::Guest => "an unidentified speaker".to_string(),
                        // Unreachable while `owner_id` is total over the enum,
                        // and stated rather than `unreachable!()` so a variant
                        // added tomorrow refuses instead of panicking.
                        ProfileScope::Owner(_) => "no one".to_string(),
                    },
                }),
            }
        }

        /// The member this proposal is for.
        pub fn profile_id(&self) -> &str {
            &self.0
        }

        /// The read scope this audience implies, for the call sites that speak
        /// the lattice. Always [`ProfileScope::Owner`]; there is no value of
        /// this type that could produce anything else.
        pub fn scope(&self) -> ProfileScope {
            ProfileScope::Owner(self.0.clone())
        }
    }

    impl TryFrom<String> for ProposalAudience {
        type Error = ProposalError;

        fn try_from(s: String) -> Result<Self, Self::Error> {
            Self::for_member(s)
        }
    }

    impl From<ProposalAudience> for String {
        fn from(a: ProposalAudience) -> String {
            a.0
        }
    }
}

pub use self::audience::ProposalAudience;
use crate::user_data::domain::profile::ProfileScope;

// ── The proposal ────────────────────────────────────────────────────────────

/// A proactive suggestion awaiting one member's approval.
///
/// Every field is private and the only constructor validates, so a `Proposal`
/// that exists is one with a rationale, an addressable audience, a confidence
/// in range, and an expiry inside [`MAX_PROPOSAL_TTL`]. There is deliberately
/// **no `Deserialize`**: a derive would be a second constructor that skips all
/// four checks, and the stored form is what an attacker or a bug would reach
/// first. Persistence reassembles through [`from_parts`](Self::from_parts).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Proposal {
    id: String,
    trigger: BusEventRef,
    rationale: String,
    proposed_action: TaskKind,
    audience: ProposalAudience,
    confidence: f32,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// The parts of a [`Proposal`] the `drafts` table has no column for.
///
/// Everything else — id, rationale, audience, expiry, creation time — is a real
/// column, so it has exactly one home. This split is not tidiness: a field
/// stored in a column *and* inside the payload JSON has two sources of truth
/// that drift, and `rationale` is the one field whose emptiness the database
/// itself must be able to refuse. It cannot refuse what it cannot see.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalPayload {
    pub trigger: BusEventRef,
    pub proposed_action: TaskKind,
    pub confidence: f32,
}

impl Proposal {
    /// Build and validate a proposal.
    ///
    /// This is both the production constructor and the rehydration path, on
    /// purpose. A separate "trust the database" path is how a validated type
    /// acquires an unvalidated back door.
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        id: impl Into<String>,
        trigger: BusEventRef,
        rationale: impl Into<String>,
        proposed_action: TaskKind,
        audience: ProposalAudience,
        confidence: f32,
        created_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, ProposalError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ProposalError::MissingId);
        }
        let rationale = rationale.into();
        if rationale.trim().is_empty() {
            return Err(ProposalError::MissingRationale { id });
        }
        if !(confidence.is_finite() && (0.0..=1.0).contains(&confidence)) {
            return Err(ProposalError::ConfidenceOutOfRange {
                id,
                requested: confidence,
            });
        }
        if expires_at <= created_at {
            return Err(ProposalError::AlreadyExpired { id });
        }
        if expires_at - created_at > MAX_PROPOSAL_TTL {
            return Err(ProposalError::TtlTooLong {
                id,
                hours: MAX_PROPOSAL_TTL.num_hours(),
            });
        }
        Ok(Self {
            id: id.trim().to_string(),
            trigger,
            rationale: rationale.trim().to_string(),
            proposed_action,
            audience,
            confidence,
            created_at,
            expires_at,
        })
    }

    /// Build a proposal that expires `ttl` after it was created.
    #[allow(clippy::too_many_arguments)]
    pub fn expiring_after(
        id: impl Into<String>,
        trigger: BusEventRef,
        rationale: impl Into<String>,
        proposed_action: TaskKind,
        audience: ProposalAudience,
        confidence: f32,
        created_at: DateTime<Utc>,
        ttl: Duration,
    ) -> Result<Self, ProposalError> {
        Self::from_parts(
            id,
            trigger,
            rationale,
            proposed_action,
            audience,
            confidence,
            created_at,
            created_at + ttl,
        )
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn trigger(&self) -> &BusEventRef {
        &self.trigger
    }

    /// Why GIAP thinks this matters. Never empty; see the module docs.
    pub fn rationale(&self) -> &str {
        &self.rationale
    }

    pub fn proposed_action(&self) -> &TaskKind {
        &self.proposed_action
    }

    pub fn audience(&self) -> &ProposalAudience {
        &self.audience
    }

    /// The read scope this proposal is addressed to. Always
    /// [`ProfileScope::Owner`].
    pub fn scope(&self) -> ProfileScope {
        self.audience.scope()
    }

    pub fn confidence(&self) -> f32 {
        self.confidence
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    /// Invariant 7, as a predicate. `expires_at` is exclusive: a proposal is
    /// dead at the instant it expires, not one tick after.
    pub fn is_live_at(&self, now: DateTime<Utc>) -> bool {
        now < self.expires_at
    }

    /// The parts that go into the `drafts.payload` column.
    pub fn payload(&self) -> ProposalPayload {
        ProposalPayload {
            trigger: self.trigger.clone(),
            proposed_action: self.proposed_action.clone(),
            confidence: self.confidence,
        }
    }

    /// The one-line description a draft listing shows.
    ///
    /// It describes the ACTION, not the reason. The rationale is a separate,
    /// always-shown field, and folding it in here would let a surface that
    /// renders only the summary look like it honours invariant 2 while showing
    /// a truncated half of it.
    pub fn summary(&self) -> String {
        match &self.proposed_action {
            TaskKind::AgentPrompt { prompt } => truncate(prompt, 120),
            TaskKind::Webhook { webhook_url } => format!("POST to {}", truncate(webhook_url, 100)),
            TaskKind::SensorTrigger(spec) => format!(
                "watch {} for {}",
                spec.source.device_id.as_deref().unwrap_or("any device"),
                spec.source.signal.as_deref().unwrap_or("any signal")
            ),
        }
    }
}

/// Char-safe truncation. Byte slicing a multi-byte prompt panics, and a
/// proposal's summary is arbitrary model output.
fn truncate(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{head}...")
}

/// Why a proposal could not be built.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ProposalError {
    #[error("a proposal must have an id")]
    MissingId,
    #[error("proposal `{id}` has no rationale, and a proposal without one may not be shown")]
    MissingRationale { id: String },
    #[error("a proposal's trigger must name an event kind")]
    MissingTriggerKind,
    #[error("proposal `{id}` has a confidence of {requested}; it must be in [0.0, 1.0]")]
    ConfidenceOutOfRange { id: String, requested: f32 },
    #[error("proposal `{id}` expires at or before it was created")]
    AlreadyExpired { id: String },
    #[error("proposal `{id}` would live longer than the {hours}h ceiling")]
    TtlTooLong { id: String, hours: i64 },
    #[error("a proposal cannot be addressed to {scope}: it must name one household member")]
    UnaddressableAudience { scope: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::profile::{ProfileScope, EXEMPLAR_OWNER_ID};

    fn trigger() -> BusEventRef {
        BusEventRef::new(
            "camera",
            Some("front-door".into()),
            Some("person".into()),
            Utc::now(),
        )
        .unwrap()
    }

    fn audience() -> ProposalAudience {
        ProposalAudience::for_member(EXEMPLAR_OWNER_ID).unwrap()
    }

    fn action() -> TaskKind {
        TaskKind::AgentPrompt {
            prompt: "tell me about the delivery".into(),
        }
    }

    fn valid(now: DateTime<Utc>) -> Proposal {
        Proposal::expiring_after(
            "prop-1",
            trigger(),
            "the delivery window closes at six",
            action(),
            audience(),
            0.8,
            now,
            Duration::hours(2),
        )
        .unwrap()
    }

    // ── Invariant 2: the rationale is mandatory ────────────────────────────

    #[test]
    fn a_blank_rationale_is_refused_in_every_blank_shape() {
        let now = Utc::now();
        for blank in ["", " ", "\t", "\n  \n"] {
            let err = Proposal::expiring_after(
                "prop-1",
                trigger(),
                blank,
                action(),
                audience(),
                0.5,
                now,
                Duration::hours(1),
            )
            .expect_err(&format!(
                "a rationale of {blank:?} must be refused: invariant 2 says every proposal \
                 carries a reason the user can read, and a blank one is what a defaulted \
                 field produces"
            ));
            assert!(
                matches!(err, ProposalError::MissingRationale { .. }),
                "a rationale of {blank:?} must be refused as MissingRationale, got {err:?}"
            );
        }
    }

    /// The reason `Proposal` derives no `Deserialize`: a derive would be a
    /// second constructor that skips every check above. This test cannot fail
    /// at runtime — it fails to COMPILE the day somebody adds the derive,
    /// because the assertion would then be false but the type would gain a
    /// trait impl. So it is written as a trait-absence check instead.
    #[test]
    fn the_only_way_to_hold_a_proposal_is_the_validating_constructor() {
        fn is_deserializable<T: for<'de> serde::Deserialize<'de>>() -> bool {
            true
        }
        // ProposalPayload IS deserializable -- it is the wire half and carries
        // no invariant of its own beyond what `from_parts` re-checks.
        assert!(is_deserializable::<ProposalPayload>());
        // If you are here because you just added `Deserialize` to `Proposal`:
        // that derive fills private fields without calling `from_parts`, so an
        // empty rationale, a confidence of 9.0 and an expiry in the year 3000
        // all become representable. Add a `#[serde(try_from = ...)]` wire
        // struct instead, the way `BusEventRef` does.
        let src = include_str!("proposal.rs");
        assert!(
            !derive_list_of(src, "Proposal").contains("Deserialize"),
            "Proposal must not derive Deserialize: it would bypass from_parts. \
             Derive list was: {}",
            derive_list_of(src, "Proposal")
        );
        // Vacuity control, and it has to go through the SAME helper. An earlier
        // version of this test had its own copy of the search for the control,
        // so a typo in the main search key left `split` returning the whole
        // file and `rsplit` returning whatever derive happened to be last --
        // and the control, using its own correct key, still passed. It failed
        // only because the tail of this file happens to contain the word
        // "Deserialize". That is luck, not a guard. `derive_list_of` panics
        // when the key matches nothing, and both calls share it.
        assert!(
            derive_list_of(src, "ProposalPayload").contains("Deserialize"),
            "the derive-list search is broken: it cannot see ProposalPayload's \
             own Deserialize. Found: {}",
            derive_list_of(src, "ProposalPayload")
        );
    }

    /// The `#[derive(...)]` list immediately above `pub struct <name> {`.
    ///
    /// Panics when the declaration is not found, which is the whole reason this
    /// is a function: a `split` on a key that matches nothing returns the input
    /// unchanged rather than failing, so a typo in a caller would silently
    /// search the wrong text.
    fn derive_list_of(src: &str, type_name: &str) -> String {
        let decl = format!("\npub struct {type_name} {{");
        let prefix = src.split(&decl).next().expect("split yields one part");
        assert!(
            prefix.len() < src.len(),
            "no declaration of `{type_name}` in this file: the search key is wrong \
             or the type was renamed"
        );
        prefix
            .rsplit("#[derive(")
            .next()
            .unwrap_or_else(|| panic!("`{type_name}` has no derive list above it"))
            .to_string()
    }

    // ── Invariants 4 and 5: addressed to a member, never broadcast ─────────

    #[test]
    fn no_scope_but_owner_can_address_a_proposal() {
        for scope in ProfileScope::every_shape() {
            let built = ProposalAudience::from_scope(&scope);
            match &scope {
                ProfileScope::Owner(id) => {
                    assert_eq!(
                        built.expect("an owner must be addressable").profile_id(),
                        id
                    );
                }
                other => {
                    let err = built.expect_err(&format!(
                        "{other:?} must not be able to hold a proposal: invariant 4 forbids the \
                         broadcast and invariant 5 forbids the guest"
                    ));
                    assert!(matches!(err, ProposalError::UnaddressableAudience { .. }));
                }
            }
        }
    }

    /// Quantified over `every_shape` rather than over an array literal of
    /// today's three variants, so a scope added tomorrow is covered on the day
    /// it is added. The assertion is the interesting half: whatever the shape,
    /// the audience it produces addresses exactly one member.
    #[test]
    fn every_audience_that_exists_names_one_member() {
        for scope in ProfileScope::every_shape() {
            if let Ok(a) = ProposalAudience::from_scope(&scope) {
                assert!(!a.profile_id().is_empty());
                assert_eq!(a.scope(), ProfileScope::Owner(a.profile_id().to_string()));
                assert!(
                    !matches!(a.scope(), ProfileScope::Household | ProfileScope::Guest),
                    "an audience resolved to a broadcast"
                );
            }
        }
    }

    #[test]
    fn an_audience_cannot_be_blank() {
        assert!(matches!(
            ProposalAudience::for_member("   ").unwrap_err(),
            ProposalError::UnaddressableAudience { .. }
        ));
    }

    #[test]
    fn an_audience_round_trips_through_serde_as_a_bare_id() {
        let a = ProposalAudience::for_member("liz").unwrap();
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, "\"liz\"");
        let back: ProposalAudience = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
        // And the serde door is the same door: a blank id is refused there too.
        assert!(serde_json::from_str::<ProposalAudience>("\"  \"").is_err());
    }

    // ── Invariant 7: it expires, and the ceiling is real ───────────────────

    #[test]
    fn a_proposal_is_dead_at_its_expiry_not_after_it() {
        let now = Utc::now();
        let p = valid(now);
        assert!(p.is_live_at(now));
        assert!(p.is_live_at(p.expires_at() - Duration::seconds(1)));
        assert!(
            !p.is_live_at(p.expires_at()),
            "expires_at is exclusive: a proposal is dead at the instant it expires"
        );
        assert!(!p.is_live_at(p.expires_at() + Duration::seconds(1)));
    }

    #[test]
    fn an_expiry_beyond_the_ceiling_is_refused() {
        let now = Utc::now();
        let err = Proposal::expiring_after(
            "prop-1",
            trigger(),
            "why",
            action(),
            audience(),
            0.5,
            now,
            MAX_PROPOSAL_TTL + Duration::seconds(1),
        )
        .unwrap_err();
        assert!(
            matches!(err, ProposalError::TtlTooLong { .. }),
            "a proposal that never expires satisfies the field and fails the invariant, got {err:?}"
        );
        // The boundary itself is allowed, so the ceiling is a ceiling and not
        // an off-by-one.
        assert!(Proposal::expiring_after(
            "prop-1",
            trigger(),
            "why",
            action(),
            audience(),
            0.5,
            now,
            MAX_PROPOSAL_TTL,
        )
        .is_ok());
    }

    #[test]
    fn a_proposal_that_is_born_expired_is_refused() {
        let now = Utc::now();
        for ttl in [Duration::zero(), Duration::seconds(-1)] {
            assert!(matches!(
                Proposal::expiring_after(
                    "prop-1",
                    trigger(),
                    "why",
                    action(),
                    audience(),
                    0.5,
                    now,
                    ttl,
                )
                .unwrap_err(),
                ProposalError::AlreadyExpired { .. }
            ));
        }
    }

    // ── The rest of the constructor ────────────────────────────────────────

    #[test]
    fn confidence_must_be_a_real_number_in_range() {
        let now = Utc::now();
        for bad in [-0.01f32, 1.01, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                matches!(
                    Proposal::expiring_after(
                        "prop-1",
                        trigger(),
                        "why",
                        action(),
                        audience(),
                        bad,
                        now,
                        Duration::hours(1),
                    )
                    .unwrap_err(),
                    ProposalError::ConfidenceOutOfRange { .. }
                ),
                "confidence {bad} must be refused"
            );
        }
        for ok in [0.0f32, 0.5, 1.0] {
            assert!(Proposal::expiring_after(
                "prop-1",
                trigger(),
                "why",
                action(),
                audience(),
                ok,
                now,
                Duration::hours(1),
            )
            .is_ok());
        }
    }

    #[test]
    fn a_trigger_must_name_an_event_kind() {
        assert!(matches!(
            BusEventRef::new("  ", None, None, Utc::now()).unwrap_err(),
            ProposalError::MissingTriggerKind
        ));
    }

    /// The point of the free-string `kind`: a variant PAI-7 P1 has not written
    /// yet is already expressible here, and this module needs no edit for it.
    #[test]
    fn a_trigger_kind_the_bus_does_not_have_yet_is_expressible() {
        for kind in ["sensor", "camera", "device", "time", "presence", "ingest"] {
            let r = BusEventRef::new(kind, None, None, Utc::now()).unwrap();
            assert_eq!(r.kind(), kind);
        }
    }

    #[test]
    fn a_trigger_round_trips_through_its_wire_form() {
        let r = trigger();
        let json = serde_json::to_string(&r).unwrap();
        let back: BusEventRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        // The wire form is the same door: a blank kind is refused there too.
        assert!(serde_json::from_str::<BusEventRef>(
            r#"{"kind":"","observed_at":"2026-08-10T00:00:00Z"}"#
        )
        .is_err());
    }

    #[test]
    fn the_payload_carries_only_what_has_no_column() {
        let p = valid(Utc::now());
        let json = serde_json::to_string(&p.payload()).unwrap();
        assert!(json.contains("camera"));
        assert!(
            !json.contains("delivery window closes"),
            "the rationale must live in its own column, not in the payload: two \
             homes for one field is two things that drift, and the database can \
             only refuse an empty rationale it can see"
        );
        let back: ProposalPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p.payload());
    }

    #[test]
    fn a_summary_describes_the_action_and_never_the_reason() {
        let p = valid(Utc::now());
        assert_eq!(p.summary(), "tell me about the delivery");
        assert!(!p.summary().contains("closes at six"));
    }

    #[test]
    fn a_summary_of_a_long_multibyte_prompt_does_not_panic() {
        let now = Utc::now();
        let prompt = "\u{00e9}".repeat(400);
        let p = Proposal::expiring_after(
            "prop-1",
            trigger(),
            "why",
            TaskKind::AgentPrompt { prompt },
            audience(),
            0.5,
            now,
            Duration::hours(1),
        )
        .unwrap();
        assert_eq!(p.summary().chars().count(), 120);
        assert!(p.summary().ends_with("..."));
    }
}
