//! Deciding whose turn this is.
//!
//! [PAI-1](../../../../../docs/architecture/pai/01-identity-and-profile-boundaries.md)
//! phase P3. One function, called once per turn, that turns the evidence a
//! session carries into a [`ProfileScope`] the rest of the system can enforce.
//!
//! # Why this is a service and not a method on `SessionIdentity`
//!
//! P2 deliberately shipped no `SessionIdentity -> ProfileScope` conversion,
//! because the stored row is not the only input. The resolution order in the
//! design is paired device, then explicit, then face, then guest -- and the
//! first of those is not a property of the session at all. Putting the decision
//! here keeps every input in one place and keeps the row a record rather than a
//! verdict.
//!
//! # The rung that exists in storage and is not yet supplied
//!
//! **Until 2026-08-10 a paired device could not be resolved to a household
//! member at all**: `session_tokens` stored `device_id` and `client_id` and no
//! profile, and neither did `push_tokens`, `pairing_codes` or
//! `handshake_challenges`. PAI-1 P9 built that link --
//! [`DeviceAttribution::device_profile`] over `devices.profile_id` (migration
//! 0043), captured at pairing-code issuance so the pairing client cannot name
//! its own member.
//!
//! [`DeviceAttribution::device_profile`]: crate::user_data::ports::device_attribution::DeviceAttribution::device_profile
//!
//! **[`ResolutionInputs::paired_device_profile`] is still fed `None` by every
//! caller.** The storage exists; the handler that would populate it does not.
//! `crates/pond-infra/tests/device_profile_rung_is_not_wired_yet.rs` fails the
//! day that changes, because this rung going live changes what an unidentified
//! speaker can reach and must be a decision rather than a discovery.
//!
//! Inferring the owner from `settings.primary_profile_id` remains refused --
//! it would attribute every phone in the house to one person, which is worse
//! than admitting we do not know. A wrong attribution is the one outcome PAI-1
//! exists to prevent.

use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::session::{IdentificationSource, SessionIdentity};

/// Everything known about who is speaking, gathered once per turn.
///
/// No `Default`, and no `..Default::default()` at construction sites. Every
/// field is an input to an authorisation decision, so a caller that has not
/// thought about one should get a compile error rather than a silent `None` --
/// the same reasoning as `ContextInputs` in the context governor.
pub struct ResolutionInputs<'a> {
    /// The member owning the paired device that authenticated this request.
    ///
    /// Always `None` today, and no longer for want of a place to look it up:
    /// PAI-1 P9 built `DeviceAttribution` over `devices.profile_id`, and the
    /// remaining half is the handler that resolves the request's token to a
    /// device and asks. See the module docs.
    pub paired_device_profile: Option<&'a str>,
    /// What the session row says, from [`SessionStorage::get_session_identity`].
    ///
    /// [`SessionStorage::get_session_identity`]: crate::user_data::ports::session_storage::SessionStorage::get_session_identity
    pub session: &'a SessionIdentity,
    /// Whether this pond has more than one household member.
    ///
    /// Load-bearing for backwards compatibility -- see [`resolve`].
    pub household_has_multiple_members: bool,
}

/// The decision, with the evidence that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedIdentity {
    pub scope: ProfileScope,
    /// What the decision rested on. Never dropped: invariant 3 requires that a
    /// choice made on a 0.6 face match be distinguishable in an audit log from
    /// one made on a paired token.
    pub source: IdentificationSource,
}

/// Resolve one turn's scope from the evidence available.
///
/// # The order
///
/// 1. **Paired device** -- cryptographic, and currently unreachable.
/// 2. **Explicit** -- the member said so, or picked themselves in the UI.
/// 3. **Face** -- above the per-profile threshold, anti-spoof passed.
/// 4. **Nothing** -- see below.
///
/// The first three all produce [`ProfileScope::Owner`]. They differ only in the
/// `source` they carry, which is what a later policy decision keys on.
///
/// # What "nothing" resolves to, and why it is not simply `Guest`
///
/// A single-member pond has exactly one person in it. Every session there is
/// unattributed, because nothing has ever written an attribution, and resolving
/// those to `Guest` would take a working assistant and make it refuse to
/// remember anything about its only user. That is not a hard boundary, it is a
/// regression.
///
/// So an unidentified speaker resolves to [`ProfileScope::Household`] while the
/// pond has one member, and to [`ProfileScope::Guest`] once it has more than
/// one. The rule reads oddly until you notice what it is actually tracking:
/// **`Household` and `Guest` differ only when there is something to be excluded
/// from.** With one member the two scopes describe the same rows.
///
/// This is the honest version of "voice is a shared surface" from the design.
/// It also means adding a second household member is the moment a pond's
/// privacy posture changes, which is a real behaviour change and belongs in
/// release notes rather than in a comment.
pub fn resolve(inputs: &ResolutionInputs<'_>) -> ResolvedIdentity {
    if let Some(profile) = inputs.paired_device_profile {
        return ResolvedIdentity {
            scope: ProfileScope::Owner(profile.to_string()),
            source: IdentificationSource::PairedDevice,
        };
    }

    // Explicit and Face both live on the session row; the row's own source
    // says which, and P2's `supersedes` already ensured the stronger one won.
    if let Some(profile) = inputs.session.profile_id.as_deref() {
        match inputs.session.source {
            IdentificationSource::PairedDevice
            | IdentificationSource::Explicit
            | IdentificationSource::Face => {
                return ResolvedIdentity {
                    scope: ProfileScope::Owner(profile.to_string()),
                    source: inputs.session.source,
                };
            }
            // A profile id with no source is not evidence. It should be
            // unreachable -- `set_session_identity` always writes both -- but
            // trusting an id whose provenance is missing is exactly the
            // failure this type exists to prevent, so it falls through.
            IdentificationSource::Unknown => {}
        }
    }

    ResolvedIdentity {
        scope: if inputs.household_has_multiple_members {
            ProfileScope::Guest
        } else {
            ProfileScope::Household
        },
        source: IdentificationSource::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(source: IdentificationSource, who: Option<&str>) -> SessionIdentity {
        SessionIdentity {
            profile_id: who.map(str::to_string),
            source,
            confidence: None,
        }
    }

    fn inputs<'a>(
        paired: Option<&'a str>,
        session: &'a SessionIdentity,
        multi: bool,
    ) -> ResolutionInputs<'a> {
        ResolutionInputs {
            paired_device_profile: paired,
            session,
            household_has_multiple_members: multi,
        }
    }

    #[test]
    fn a_paired_device_outranks_whatever_the_session_row_says() {
        let s = session(IdentificationSource::Face, Some("liz"));
        let resolved = resolve(&inputs(Some("jerry"), &s, true));
        assert_eq!(resolved.scope, ProfileScope::Owner("jerry".into()));
        assert_eq!(resolved.source, IdentificationSource::PairedDevice);
    }

    #[test]
    fn an_explicit_or_face_binding_resolves_to_its_owner_and_keeps_its_source() {
        for source in [IdentificationSource::Explicit, IdentificationSource::Face] {
            let s = session(source, Some("jerry"));
            let resolved = resolve(&inputs(None, &s, true));
            assert_eq!(resolved.scope, ProfileScope::Owner("jerry".into()));
            assert_eq!(
                resolved.source, source,
                "the source must survive resolution -- invariant 3"
            );
        }
    }

    /// The compatibility rule. Every session in every existing pond is
    /// unattributed; resolving those to Guest would make a working
    /// single-user assistant refuse to remember anything about its only user.
    #[test]
    fn an_unidentified_speaker_in_a_one_member_pond_still_sees_the_household() {
        let s = SessionIdentity::unknown();
        let resolved = resolve(&inputs(None, &s, false));
        assert_eq!(resolved.scope, ProfileScope::Household);
        assert_eq!(resolved.source, IdentificationSource::Unknown);
    }

    /// And the boundary. Once there is a second member there is something to
    /// be excluded from, so an unidentified speaker is a guest.
    #[test]
    fn an_unidentified_speaker_in_a_shared_pond_is_a_guest() {
        let s = SessionIdentity::unknown();
        let resolved = resolve(&inputs(None, &s, true));
        assert_eq!(resolved.scope, ProfileScope::Guest);
        assert!(resolved.scope.excludes_everything());
    }

    /// A profile id with no provenance is not evidence. This should be
    /// unreachable, which is exactly why it is worth pinning: if a future
    /// write path forgets the source, the failure must be a guest session and
    /// not a silently trusted attribution.
    #[test]
    fn a_profile_id_without_a_source_is_not_trusted() {
        let s = session(IdentificationSource::Unknown, Some("jerry"));
        let resolved = resolve(&inputs(None, &s, true));
        assert_eq!(resolved.scope, ProfileScope::Guest);
        assert_ne!(resolved.scope, ProfileScope::Owner("jerry".into()));
    }
}
