//! The device a request was **proved** to come from, and what that proves
//! about who is speaking.
//!
//! [PAI-1](../../../../../docs/architecture/pai/01-identity-and-profile-boundaries.md)
//! P9's identity half. `identity_resolution::resolve` has honoured
//! `paired_device_profile` since 2026-08-04 and every caller fed it `None`,
//! because a turn did not carry a device id. This is the type that carries one.
//!
//! # Why a newtype and not a `String`
//!
//! `IdentificationSource::PairedDevice` is the **strongest** rung of the
//! resolver: it outranks face and explicit identification, so whatever fills it
//! outranks every proof the pond can actually make. A `&str` parameter would be
//! satisfied by `headers.get("X-Device-Id")` as readily as by the token lookup,
//! and the two are not remotely the same claim — one is a fact the pond wrote
//! down at pairing, the other is a sentence the caller typed.
//!
//! So the field is private and there are exactly two public constructors:
//! [`ProvenDevice::from_principal`], which can only read what the auth layer
//! put on the request from the token the pond issued, and
//! [`ProvenDevice::none`], which names no device at all. **There is no
//! constructor that takes a caller-supplied string**, so honouring a header
//! would require adding one to this file — a visible change to a security type,
//! rather than an invisible change to a handler.
//!
//! # Why the fall-through is a type and not an `Option`
//!
//! [`DeviceRung`] mirrors
//! [`TargetedDelivery`](crate::user_data::ports::device_attribution::TargetedDelivery)
//! deliberately. Four different things can happen when the pond asks whose
//! device this is, three of them mean "fall through to the next rung", and the
//! shortest way to write that with an `Option` is `.unwrap_or_default()` on the
//! attribution read — which turns a failed database read into "unattributed"
//! and makes a broken pond indistinguishable, in a log, from a shared tablet.
//! [`DeviceRung::from_attribution`] consumes the `Result` so the distinction
//! cannot be dropped, and only one variant carries a member.

use anyhow::Result;

use crate::security::ports::policy::Principal;

/// The device this request was proved to come from, if any.
///
/// Cheap to clone and deliberately not `Default`: "no device" is spelled
/// [`ProvenDevice::none`] at a call site that had to type it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenDevice(Option<String>);

impl ProvenDevice {
    /// The device the authenticated principal presented — the **only**
    /// constructor that can name one.
    ///
    /// `Principal::device_id` is populated in exactly one place, the API's auth
    /// middleware, from `Handshake::caller_for_token`. That is the token the
    /// pond issued at pairing, so the chain from here back to a
    /// `devices.profile_id` written by the operator is unbroken and contains
    /// nothing the client said about itself.
    pub fn from_principal(principal: &Principal) -> Self {
        Self(principal.device_id.clone())
    }

    /// No device on this request.
    ///
    /// The honest answer for an in-process caller, for the loopback dev bypass
    /// (which returns before a token is ever read), and for a request that
    /// reached a handler with no `Principal` attached at all. Every rung below
    /// the paired-device one still applies, so this narrows rather than
    /// refusing.
    pub fn none() -> Self {
        Self(None)
    }

    /// The device id to look an attribution up by, if there is one.
    pub fn id(&self) -> Option<&str> {
        self.0.as_deref()
    }

    /// Turn this request's attribution read into the resolver's strongest rung.
    ///
    /// Takes the `Result` rather than the `Option` for the reason
    /// `TargetedDelivery::plan` does: handing the caller a `Result` puts the
    /// failed read where the shortest thing to write is `.unwrap_or_default()`.
    ///
    /// The read is only performed when this device names one, so a caller with
    /// [`ProvenDevice::none`] passes `Ok(None)` — or never calls at all.
    pub fn rung(&self, attribution: Result<Option<String>>) -> DeviceRung {
        if self.0.is_none() {
            // Belt and braces: an attribution read that happened for some
            // *other* device must not be able to speak for this request.
            return DeviceRung::NoDevice;
        }
        DeviceRung::from_attribution(attribution)
    }
}

/// What the device rung had to say about who is speaking.
///
/// Exhaustive and wildcard-free at every use site on purpose: a fifth answer
/// has to decide, in the open, whether it resolves a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceRung {
    /// This device is that member's. Resolves to `Owner(id)` at
    /// `IdentificationSource::PairedDevice` strength.
    Member(String),
    /// The request carried no device — loopback, in-process, or an adapter that
    /// cannot name one. Falls through.
    NoDevice,
    /// The device is registered and nobody has claimed it. Migration 0043's
    /// NULL: *I do not know*, never *everybody*. Falls through.
    Unattributed,
    /// The attribution read itself failed. **This is the variant the type
    /// exists for.** Falls through, loudly, and never assumes a member: on a
    /// failed read access narrows (PAI-1 invariant 2).
    Unavailable(String),
}

impl DeviceRung {
    /// Classify an attribution read. Nothing else may construct
    /// [`DeviceRung::Member`] from a read.
    pub fn from_attribution(attribution: Result<Option<String>>) -> Self {
        match attribution {
            Ok(Some(profile_id)) if !profile_id.trim().is_empty() => Self::Member(profile_id),
            // A blank id is neither NULL nor a member -- `checked_profile_id`'s
            // rule, applied on the way out as well as on the way in. Resolving
            // it would produce `Owner("")`, which matches no member's rows and
            // is not `Household` either: a scope that silently means nothing.
            Ok(Some(_)) | Ok(None) => Self::Unattributed,
            Err(e) => Self::Unavailable(e.to_string()),
        }
    }

    /// The member this rung resolves, if it resolves one.
    ///
    /// This is the value
    /// [`ResolutionInputs::paired_device_profile`](crate::user_data::services::identity_resolution::ResolutionInputs::paired_device_profile)
    /// wants. Three of the four variants answer `None`, which is the whole
    /// safety property: every way of failing to identify a device falls through
    /// to the next rung.
    pub fn profile_id(&self) -> Option<&str> {
        match self {
            Self::Member(id) => Some(id),
            Self::NoDevice | Self::Unattributed | Self::Unavailable(_) => None,
        }
    }

    /// Short, stable label for structured logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Member(_) => "member",
            Self::NoDevice => "no_device",
            Self::Unattributed => "unattributed",
            Self::Unavailable(_) => "unavailable",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::ports::policy::PrincipalKind;
    use crate::user_data::domain::profile::ProfileScope;
    use crate::user_data::domain::session::{IdentificationSource, SessionIdentity};
    use crate::user_data::services::identity_resolution::{resolve, ResolutionInputs};
    use anyhow::anyhow;

    /// Vacuity control for every refusal below: if `from_attribution` answered
    /// `Unattributed` for everything, they would all pass while proving
    /// nothing.
    #[test]
    fn an_attributed_device_names_its_member() {
        let device =
            ProvenDevice::from_principal(&Principal::token("phone-liz").with_device("phone-liz"));
        let rung = device.rung(Ok(Some("liz".to_string())));
        assert_eq!(rung, DeviceRung::Member("liz".to_string()));
        assert_eq!(rung.profile_id(), Some("liz"));
    }

    /// Migration 0043's header, in the identity direction: NULL is "I do not
    /// know", so the resolver falls through instead of resolving to anybody.
    #[test]
    fn an_unattributed_device_falls_through_rather_than_resolving_anybody() {
        let device =
            ProvenDevice::from_principal(&Principal::token("tablet").with_device("tablet"));
        let rung = device.rung(Ok(None));
        assert_eq!(
            rung,
            DeviceRung::Unattributed,
            "a NULL devices.profile_id is 'no household member has claimed this'. Reading it as \
             a member is the wrong-attribution failure PAI-1 exists to prevent."
        );
        assert_eq!(
            rung.profile_id(),
            None,
            "an unclaimed device must resolve to nobody -- not to the last member who paired"
        );
    }

    /// A blank `profile_id` is not a member. `Owner(\"\")` would match no rows
    /// and would not be `Household` either, so it is a scope that silently
    /// means nothing while looking like an identification.
    #[test]
    fn a_blank_profile_id_is_not_a_member() {
        for blank in ["", "   ", "\t"] {
            let device =
                ProvenDevice::from_principal(&Principal::token("phone").with_device("phone"));
            assert_eq!(
                device.rung(Ok(Some(blank.to_string()))),
                DeviceRung::Unattributed,
                "a blank profile id must not become Owner({blank:?})"
            );
        }
    }

    /// The direction that costs a privacy failure when it is wrong. An
    /// unreadable store must refuse to identify, never fall back to a
    /// permissive default.
    #[test]
    fn a_failed_attribution_read_narrows_and_says_which_failure_it_was() {
        let device = ProvenDevice::from_principal(&Principal::token("phone").with_device("phone"));
        let rung = device.rung(Err(anyhow!("database is locked")));
        match &rung {
            DeviceRung::Unavailable(why) => assert!(
                why.contains("database is locked"),
                "the storage error must survive into the rung, got: {why}"
            ),
            other => panic!(
                "a failed attribution read must be Unavailable, not {other:?}. Answering it with \
                 a member assumes an identity the pond could not read, and answering it with \
                 Unattributed makes a broken database look exactly like a shared tablet"
            ),
        }
        assert_eq!(
            rung.profile_id(),
            None,
            "a failed read must resolve nobody at PairedDevice strength"
        );
    }

    /// A request with no device cannot be spoken for by somebody else's
    /// attribution read.
    #[test]
    fn a_request_with_no_device_resolves_no_member_however_the_read_went() {
        let device = ProvenDevice::none();
        assert_eq!(device.id(), None);
        assert_eq!(
            device.rung(Ok(Some("liz".to_string()))),
            DeviceRung::NoDevice
        );
        assert_eq!(device.rung(Ok(None)), DeviceRung::NoDevice);
        assert_eq!(device.rung(Err(anyhow!("boom"))), DeviceRung::NoDevice);
    }

    /// A principal that authenticated but carries no device — the loopback dev
    /// bypass, an in-process caller, or an adapter that cannot name one.
    #[test]
    fn a_principal_without_a_device_names_none() {
        for principal in [
            Principal::loopback(),
            Principal::internal(),
            Principal::token("gotg-1"),
        ] {
            assert_eq!(
                ProvenDevice::from_principal(&principal).id(),
                None,
                "{:?} carries no device and must not invent one",
                principal.kind
            );
        }
    }

    /// Structural, and the reason this is a newtype at all. The field is
    /// private and these are the only two public constructors, so a handler
    /// that wanted to honour `X-Device-Id` has nothing to call: there is no
    /// `From<String>`, no `new(&str)`, no `Default`. This test is the executable
    /// note that adding one is the change to argue about.
    #[test]
    fn the_only_way_to_name_a_device_is_from_a_principal() {
        let from_client_input = "attacker-chosen-device-id";
        // The only two ways to build one, exhaustively:
        assert_eq!(ProvenDevice::none().id(), None);
        assert_eq!(
            ProvenDevice::from_principal(&Principal::token("c").with_device(from_client_input))
                .id(),
            Some(from_client_input),
            "and even this one can only echo what the auth middleware put on the Principal"
        );
        // Where that value comes from is the middleware's business, and it is
        // guarded there: `crates/pond-infra/tests/device_rung_wiring.rs` asserts
        // `with_device` is called exactly once in the workspace, on the token
        // path, with the handshake lookup's own answer.
    }

    /// Every rung must be distinguishable in the one place an operator looks.
    #[test]
    fn every_rung_has_its_own_log_label() {
        let labels = [
            DeviceRung::Member("liz".into()).as_str(),
            DeviceRung::NoDevice.as_str(),
            DeviceRung::Unattributed.as_str(),
            DeviceRung::Unavailable("x".into()).as_str(),
        ];
        let unique: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
        assert_eq!(
            unique.len(),
            labels.len(),
            "two rungs sharing a label make two different failures indistinguishable: {labels:?}"
        );
    }

    /// The rung, joined to the resolver it exists to feed. `PairedDevice`
    /// outranks explicit identification, so an attributed device wins even
    /// against a session somebody else bound.
    #[test]
    fn an_attributed_device_outranks_the_session_binding() {
        let device = ProvenDevice::from_principal(&Principal::token("p").with_device("p"));
        let rung = device.rung(Ok(Some("liz".to_string())));

        let mut session = SessionIdentity::unknown();
        session.profile_id = Some("jerry".to_string());
        session.source = IdentificationSource::Explicit;

        let resolved = resolve(&ResolutionInputs {
            paired_device_profile: rung.profile_id(),
            session: &session,
            household_has_multiple_members: true,
        });
        assert_eq!(resolved.scope, ProfileScope::Owner("liz".to_string()));
        assert_eq!(resolved.source, IdentificationSource::PairedDevice);
    }

    /// And the fall-through, at the same seam: an unattributed device leaves
    /// the session binding standing rather than overriding it with nobody.
    #[test]
    fn an_unattributed_device_leaves_the_next_rung_standing() {
        let device = ProvenDevice::from_principal(&Principal::token("t").with_device("t"));
        let rung = device.rung(Ok(None));

        let mut session = SessionIdentity::unknown();
        session.profile_id = Some("jerry".to_string());
        session.source = IdentificationSource::Explicit;

        let resolved = resolve(&ResolutionInputs {
            paired_device_profile: rung.profile_id(),
            session: &session,
            household_has_multiple_members: true,
        });
        assert_eq!(
            resolved.scope,
            ProfileScope::Owner("jerry".to_string()),
            "an unattributed device must not override the member this session was bound to; \
             falling through means leaving the next rung standing, not answering over it"
        );
        assert_eq!(resolved.source, IdentificationSource::Explicit);
    }

    /// A failed read must not promote a guest, and must not demote a member who
    /// identified themselves some other way either. It simply is not evidence.
    #[test]
    fn a_failed_read_leaves_an_unidentified_speaker_exactly_where_it_found_them() {
        let device = ProvenDevice::from_principal(&Principal::token("p").with_device("p"));
        let rung = device.rung(Err(anyhow!("disk gone")));
        let resolved = resolve(&ResolutionInputs {
            paired_device_profile: rung.profile_id(),
            session: &SessionIdentity::unknown(),
            household_has_multiple_members: true,
        });
        assert_eq!(
            resolved.scope,
            ProfileScope::Guest,
            "an unreadable attribution store must leave a stranger a stranger"
        );
        assert_eq!(resolved.source, IdentificationSource::Unknown);
    }

    #[test]
    fn a_token_principal_keeps_its_kind_when_a_device_is_attached() {
        let principal = Principal::token("gotg-7").with_device("gotg-7");
        assert_eq!(principal.kind, PrincipalKind::Token("gotg-7".to_string()));
        assert_eq!(principal.device_id.as_deref(), Some("gotg-7"));
        assert_eq!(
            principal.proven_profile_id, None,
            "attaching a device must not, by itself, prove a member"
        );
    }
}
