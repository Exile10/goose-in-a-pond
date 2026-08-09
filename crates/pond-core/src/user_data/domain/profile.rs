//! Profile domain types — represents a household member.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A household member profile.
///
/// `preferences` is a flat string-to-string map (avoids `serde_json::Value`
/// dependency in pond-core). Callers in pond-api convert to/from JSON objects
/// when crossing the HTTP boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub display_name: String,
    /// Single emoji representing this profile (default: duck emoji)
    pub avatar_emoji: String,
    /// Arbitrary string key-value preferences
    pub preferences: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Whose data a read or write is scoped to.
///
/// This replaces the `Option<&str>` profile filter that every repository method
/// used to take. The problem with `Option<String>` for an owner is not that it
/// is imprecise — it is that `None` is always available, so it becomes the
/// value every call site passes. Before this type existed, **every** production
/// caller passed `None`, and the household shared one memory pool. A closed enum
/// forces each call site to say which of three different things it means.
///
/// # Semantics
///
/// - [`Owner`](Self::Owner) — this person's own data, plus anything unattributed
///   (`profile_id IS NULL`), which is shared household context.
/// - [`Household`](Self::Household) — everything, unfiltered. This is what a
///   single-member pond has always done, so it is the migration-safe default.
/// - [`Guest`](Self::Guest) — nothing. An unidentified speaker gets no personal
///   data at all, and that is the whole point of the variant.
///
/// # Note for PAI-1 phase P1
///
/// P1 introduces the type and changes signatures only: every call site passes
/// [`Household`](Self::Household), whose SQL is byte-identical to the old
/// `None` branch. Nothing changes behaviourally until the resolution chain lands
/// in P3 and enforcement in P4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileScope {
    /// A specific household member, plus unattributed shared rows.
    Owner(String),
    /// The whole household, unfiltered.
    Household,
    /// An unidentified speaker. Sees no personal data.
    Guest,
}

/// The owner id [`ProfileScope::every_shape`] puts inside its `Owner`.
///
/// Arbitrary — `Owner` needs a payload and a guard quantifying over shapes does
/// not care which member. A guard that needs TWO owners takes the second from
/// [`SECOND_EXEMPLAR_OWNER_ID`], because two members are incomparable rather
/// than ordered (see [`ProfileScope::is_within`]).
pub const EXEMPLAR_OWNER_ID: &str = "exemplar-owner";

/// A second owner id, DIFFERENT from [`EXEMPLAR_OWNER_ID`], for the guards that
/// need an incomparable pair.
///
/// It lives beside `every_shape`'s id rather than inside each test module for
/// one reason: two copies of "a-different-member" can silently converge, and a
/// fixture whose two owners are the same member turns every
/// "two members are incomparable" assertion into a scope compared with itself.
/// PAI-6's scope-inheritance sweep had its own copy, and rewriting that copy to
/// [`EXEMPLAR_OWNER_ID`] left all 899 pond-core tests green (verified by
/// mutation, 2026-08-09). One constant means one distinctness guard —
/// `scope_lattice_tests::the_second_owner_really_is_a_different_member` — covers
/// every fixture that uses it.
pub const SECOND_EXEMPLAR_OWNER_ID: &str = "a-different-member";

impl ProfileScope {
    /// One value of every shape this enum has.
    ///
    /// A guard that quantifies over scopes builds its fixture from here rather
    /// than writing out today's three variants. That is not tidiness: an array
    /// literal of today's shapes cannot see a shape added tomorrow, and PAI-6's
    /// four scope-inheritance guards each carried their own copy of one —
    /// twenty lines after the commit that had removed exactly that defect from
    /// `RolePersonalData`.
    ///
    /// `Owner` carries a `String`, so this is a function and not a `const ALL`
    /// like [`RedactionKind::ALL`] or [`OnboardingStep::ALL`].
    ///
    /// **What this list is and is not.** It is coverage: it decides which
    /// inputs the guards see. It is NOT the enforcement of PAI-6 invariant 1 —
    /// a list cannot be, because a variant can be added without touching it.
    /// The enforcement is `orchestration::ChildScope`, which clamps any
    /// candidate the parent does not contain down to [`Guest`](Self::Guest),
    /// whatever shape the candidate turned out to be, and which is a type
    /// rather than a function so that the clamp cannot be unwired from its one
    /// call site.
    ///
    /// [`RedactionKind::ALL`]: crate::security::domain::redaction::RedactionKind::ALL
    /// [`OnboardingStep::ALL`]: crate::user_data::domain::onboarding::OnboardingStep::ALL
    pub fn every_shape() -> Vec<ProfileScope> {
        vec![
            ProfileScope::Owner(EXEMPLAR_OWNER_ID.to_string()),
            ProfileScope::Household,
            ProfileScope::Guest,
        ]
    }

    /// Named constructor for `serde(default)` attributes.
    ///
    /// Only for deserializing a payload written before a scope field existed.
    /// Never reach for this in Rust code -- state the scope you mean.
    pub fn household() -> Self {
        ProfileScope::Household
    }

    /// The profile id to filter on, when the scope narrows to one person.
    ///
    /// `Household` and `Guest` both return `None`, for opposite reasons —
    /// `Household` because it wants everything and `Guest` because it wants
    /// nothing — so this must never be the only thing a caller checks. Pair it
    /// with [`excludes_everything`](Self::excludes_everything).
    pub fn owner_id(&self) -> Option<&str> {
        match self {
            ProfileScope::Owner(id) => Some(id.as_str()),
            ProfileScope::Household | ProfileScope::Guest => None,
        }
    }

    /// True when the scope can never match a row, so a query need not run.
    pub fn excludes_everything(&self) -> bool {
        matches!(self, ProfileScope::Guest)
    }

    /// True when personal data may be surfaced to this scope at all.
    pub fn allows_personal_data(&self) -> bool {
        !self.excludes_everything()
    }

    /// True when `self` can reach no row that `wider` cannot.
    ///
    /// The lattice, which is a PARTIAL order and is easy to get wrong:
    ///
    /// - [`Guest`](Self::Guest) is within everything -- it reaches nothing.
    /// - [`Owner(x)`](Self::Owner) is within `Owner(x)` and within
    ///   [`Household`](Self::Household).
    /// - `Owner(x)` and `Owner(y)` are INCOMPARABLE for `x != y`. Neither is
    ///   within the other, because each reaches rows the other cannot.
    /// - `Household` is within only `Household`.
    ///
    /// Written for PAI-6 invariant 1 -- "a subagent's scope is a subset of its
    /// parent's, never wider" -- but it belongs on the type rather than in the
    /// orchestration module, because it is a property of the scope lattice and
    /// PAI-6 P3 is not the only thing that will need to ask.
    ///
    /// Note that `Owner(x).is_within(&Owner(x))` and
    /// `Household.is_within(&Household)` are both true: "never wider" permits
    /// equal, which is what inheriting a scope unchanged means.
    pub fn is_within(&self, wider: &ProfileScope) -> bool {
        match (self, wider) {
            // Reaches nothing, so it is within anything.
            (ProfileScope::Guest, _) => true,
            // Reaches everything, so only Household contains it.
            (ProfileScope::Household, ProfileScope::Household) => true,
            (ProfileScope::Household, _) => false,
            (ProfileScope::Owner(_), ProfileScope::Household) => true,
            (ProfileScope::Owner(a), ProfileScope::Owner(b)) => a == b,
            (ProfileScope::Owner(_), ProfileScope::Guest) => false,
        }
    }
}

/// Request body for creating a new profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProfileRequest {
    pub display_name: String,
    #[serde(default = "default_avatar")]
    pub avatar_emoji: String,
}

fn default_avatar() -> String {
    "\u{1F986}".to_string() // 🦆
}

#[cfg(test)]
mod profile_scope_tests {
    use super::*;

    #[test]
    fn owner_exposes_its_id_and_nothing_else_does() {
        assert_eq!(
            ProfileScope::Owner("jerry".into()).owner_id(),
            Some("jerry")
        );
        assert_eq!(ProfileScope::Household.owner_id(), None);
        assert_eq!(ProfileScope::Guest.owner_id(), None);
    }

    /// `owner_id()` returns `None` for Household AND Guest, for opposite
    /// reasons. Anything that branches on it alone would hand a Guest the whole
    /// household's memory, which is the exact failure this type exists to
    /// prevent.
    #[test]
    fn household_and_guest_are_not_interchangeable_despite_both_lacking_an_id() {
        assert_eq!(
            ProfileScope::Household.owner_id(),
            ProfileScope::Guest.owner_id()
        );
        assert!(!ProfileScope::Household.excludes_everything());
        assert!(ProfileScope::Guest.excludes_everything());
        assert!(ProfileScope::Household.allows_personal_data());
        assert!(!ProfileScope::Guest.allows_personal_data());
    }

    #[test]
    fn owner_sees_personal_data() {
        let owner = ProfileScope::Owner("jerry".into());
        assert!(!owner.excludes_everything());
        assert!(owner.allows_personal_data());
    }
}

#[cfg(test)]
mod scope_lattice_tests {
    use super::*;

    /// Every shape, plus a SECOND owner.
    ///
    /// The second one is not decoration: `is_within` is a partial order and the
    /// pair it is most likely to get wrong is two different members, which a
    /// fixture holding one `Owner` cannot produce.
    /// [`ProfileScope::every_shape`] deliberately supplies only one, so this is
    /// where the second is added — and
    /// `the_second_owner_really_is_a_different_member` below fails if the two
    /// ids ever converge, which would silently delete the incomparable case
    /// from three tests at once.
    fn every_scope() -> Vec<ProfileScope> {
        let mut scopes = ProfileScope::every_shape();
        scopes.push(ProfileScope::Owner(SECOND_EXEMPLAR_OWNER_ID.to_string()));
        scopes
    }

    /// The distinctness guard for BOTH fixtures that hold two owners: this
    /// module's [`every_scope`], and PAI-6's
    /// `orchestration::scope_inheritance_tests::candidate_scopes`. That second
    /// one used to carry its own copy of the string, so this guard could not
    /// see it — and rewriting that copy to [`EXEMPLAR_OWNER_ID`] left the whole
    /// suite green while deleting the incomparable pair from the clamp's sweep.
    /// Both now read [`SECOND_EXEMPLAR_OWNER_ID`], which is why this assertion
    /// is worth more than it looks.
    #[test]
    fn the_second_owner_really_is_a_different_member() {
        assert_ne!(
            SECOND_EXEMPLAR_OWNER_ID, EXEMPLAR_OWNER_ID,
            "the fixture's two owners are the same member, so every 'two members are \
             incomparable' assertion below is comparing a scope with itself -- and so is \
             PAI-6's scope-inheritance sweep, which reads the same constant"
        );
        let owners = every_scope()
            .iter()
            .filter(|s| matches!(s, ProfileScope::Owner(_)))
            .count();
        assert_eq!(
            owners, 2,
            "the lattice fixture holds {owners} owners; with fewer than two, \
             `two_owners_are_incomparable` and the refused-pair count below stop \
             exercising the partial order at all"
        );
    }

    /// `every_shape()` decides which inputs every scope guard in this crate
    /// sees, so a variant missing from it is a variant nothing is quantified
    /// over. Derive the truth from the enum rather than from a second
    /// hand-written list: serde's `unknown variant` error names every variant
    /// the generated `Deserialize` impl knows about.
    ///
    /// This net has a known hole — a variant carrying `#[serde(skip)]` is
    /// invisible to it — and that hole is why the profile axis is not defended
    /// by this list at all. It is defended by `orchestration::ChildScope`,
    /// which clamps rather than enumerates. Read this as coverage breadth, not
    /// as the boundary.
    #[test]
    fn every_shape_lists_every_variant_the_enum_has() {
        let message = serde_json::from_str::<ProfileScope>("\"definitely_not_a_variant\"")
            .expect_err("that is not a variant")
            .to_string();
        let from_the_enum: Vec<&str> = message.split('`').skip(1).step_by(2).skip(1).collect();
        assert!(
            !from_the_enum.is_empty(),
            "no variant names could be read out of serde's message {message:?}, so this test \
             can no longer see a new variant at all"
        );
        let listed: Vec<String> = ProfileScope::every_shape()
            .iter()
            .map(|s| match s {
                ProfileScope::Owner(_) => "owner".to_string(),
                ProfileScope::Household => "household".to_string(),
                ProfileScope::Guest => "guest".to_string(),
            })
            .collect();
        for variant in &from_the_enum {
            assert!(
                listed.iter().any(|l| l == variant),
                "ProfileScope has a variant `{variant}` that every_shape() does not produce. \
                 Every scope guard in this crate quantifies over that list, so an unlisted \
                 shape is one nothing is tested against"
            );
        }
        assert_eq!(
            from_the_enum.len(),
            listed.len(),
            "every_shape() and the enum disagree on how many shapes exist: {from_the_enum:?} \
             against {listed:?}"
        );
    }

    /// Reflexive: inheriting a scope unchanged is "never wider".
    #[test]
    fn every_scope_is_within_itself() {
        for s in every_scope() {
            assert!(s.is_within(&s), "{s:?} is not within itself");
        }
    }

    /// The whole point. `Household` reaches every row, so nothing narrower
    /// contains it -- if this ever returns true, a Guest turn could delegate to
    /// a subagent that reads the household's memory.
    #[test]
    fn household_is_within_nothing_narrower() {
        assert!(!ProfileScope::Household.is_within(&ProfileScope::Guest));
        assert!(!ProfileScope::Household.is_within(&ProfileScope::Owner("jerry".into())));
    }

    /// Two members are incomparable. This is the case a total order gets wrong:
    /// Liz's scope is not "smaller" than Jerry's, it is elsewhere.
    #[test]
    fn two_owners_are_incomparable() {
        let jerry = ProfileScope::Owner("jerry".into());
        let liz = ProfileScope::Owner("liz".into());
        assert!(!jerry.is_within(&liz));
        assert!(!liz.is_within(&jerry));
    }

    #[test]
    fn guest_is_within_everything_and_only_guest_is_within_guest() {
        for wider in every_scope() {
            assert!(
                ProfileScope::Guest.is_within(&wider),
                "Guest should be within {wider:?}"
            );
        }
        for narrower in every_scope() {
            let expected = matches!(narrower, ProfileScope::Guest);
            assert_eq!(
                narrower.is_within(&ProfileScope::Guest),
                expected,
                "{narrower:?} within Guest should be {expected}"
            );
        }
    }

    #[test]
    fn an_owner_is_within_the_household() {
        assert!(ProfileScope::Owner("jerry".into()).is_within(&ProfileScope::Household));
    }

    /// Vacuity control: the predicate must not be a constant. If it ever
    /// returned `true` unconditionally -- the widening direction, and the one a
    /// careless simplification lands on -- every other test here would still
    /// pass except the negative ones, so pin the count of false answers too.
    #[test]
    fn the_predicate_refuses_a_specific_number_of_pairs() {
        let scopes = every_scope();
        let refused = scopes
            .iter()
            .flat_map(|a| scopes.iter().map(move |b| (a, b)))
            .filter(|(a, b)| !a.is_within(b))
            .count();
        // 16 ordered pairs. Permitted: 4 reflexive, Guest within the other 3,
        // and 2 owners within Household = 4 + 3 + 2 = 9. So 7 are refused.
        assert_eq!(
            refused, 7,
            "the scope lattice changed shape; if that was deliberate, say which \
             pair moved and why it is not a widening"
        );
    }
}

#[cfg(test)]
mod scope_gating_tests {
    use super::*;

    /// The predicate the adapter's memory-injection gate is written against
    /// (`goose_agent.rs`, `memory_limit`). Pinned here because that gate is a
    /// boolean `&&` in a crate whose tests cannot construct a live turn, so
    /// this is where the meaning is defended.
    #[test]
    fn only_a_guest_is_denied_personal_data() {
        assert!(ProfileScope::Owner("jerry".into()).allows_personal_data());
        assert!(ProfileScope::Household.allows_personal_data());
        assert!(!ProfileScope::Guest.allows_personal_data());
    }

    /// A scope must survive a serialization round trip unchanged, because it
    /// rides `AgentRequest` which is `Serialize`/`Deserialize`. A variant that
    /// silently widened across that boundary would be undetectable.
    ///
    /// The input set is [`ProfileScope::every_shape`] and not an array literal
    /// of today's three, which is what it was — recorded vacuity shape 4, and
    /// twenty lines below the commit that introduced `every_shape` to remove
    /// exactly that shape from the guards next door. A literal cannot see a
    /// variant added tomorrow, and the entry deleted from a literal fails
    /// nothing; `every_shape` is itself guarded, by
    /// `scope_lattice_tests::every_shape_lists_every_variant_the_enum_has`, so
    /// an entry deleted from IT fails there.
    #[test]
    fn every_scope_round_trips_through_serde() {
        for scope in ProfileScope::every_shape() {
            let json = serde_json::to_string(&scope).expect("scope must serialize");
            let back: ProfileScope = serde_json::from_str(&json).expect("scope must deserialize");
            assert_eq!(back, scope, "round trip changed the scope: {json}");
        }
    }
}
