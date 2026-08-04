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

impl ProfileScope {
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
    #[test]
    fn every_scope_round_trips_through_serde() {
        for scope in [
            ProfileScope::Owner("jerry".into()),
            ProfileScope::Household,
            ProfileScope::Guest,
        ] {
            let json = serde_json::to_string(&scope).expect("scope must serialize");
            let back: ProfileScope = serde_json::from_str(&json).expect("scope must deserialize");
            assert_eq!(back, scope, "round trip changed the scope: {json}");
        }
    }
}
