//! Driven port: which household member a device belongs to.
//!
//! [PAI-1](../../../../../docs/architecture/pai/01-identity-and-profile-boundaries.md)
//! -- the device-to-profile rung. This is the link
//! [`identity_resolution::resolve`] has been asking for since 2026-08-04 and
//! [PAI-7](../../../../../docs/architecture/pai/07-proactive-intelligence.md)
//! section 3.4 assumes exists: **profile -> paired devices -> push tokens**.
//!
//! [`identity_resolution::resolve`]: crate::user_data::services::identity_resolution::resolve
//!
//! # The two directions are not mirror images
//!
//! One column (`devices.profile_id`, migration 0043) answers two questions, and
//! they fail differently, so they are separate methods with separate contracts:
//!
//! * **Identity** -- [`DeviceAttribution::device_profile`]. "Whose device sent
//!   this?" `None` means *I do not know*, and the caller falls through to the
//!   next rung of the resolution chain. That is a narrowing, and it is what
//!   every caller already does today.
//! * **Delivery** -- [`DeviceAttribution::devices_for_profile`] and
//!   [`DeviceAttribution::push_tokens_for_profile`]. "Where do I reach this
//!   person?" An unattributed device is **not** an answer to that question and
//!   is never returned. PAI-7's invariant 4 is that proposals are addressed to
//!   a profile and never broadcast; a targeted send that quietly fanned out to
//!   every unclaimed screen in the house would satisfy the type signature and
//!   break the invariant.
//!
//! So an unattributed device means *deliver to nobody* here, not *deliver to
//! everybody*. Reaching a shared kitchen tablet remains the broadcast path's
//! job, which is honest about being a broadcast. This is deliberately a
//! different call from PAI-1's memory classification, where an unattributed
//! fragment reads as shared household context: a memory is content, a device is
//! a destination, and only one of those discloses something by being read by
//! the wrong person.
//!
//! # Why this is its own port
//!
//! `DeviceRegistry` has twenty-three implementors across seven crates, almost
//! all of them test doubles that answer "no devices". A required method there
//! would be answered by whoever was quickest to satisfy the compiler, and PAI-2
//! P7 already recorded what that costs: "a default on a trait that answers a
//! security question is a decision made by whoever forgot to override it."
//! Every method here is required, and there is exactly one implementor.

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::user_data::domain::push_token::PushToken;

/// The `target` string that means "every connected device".
///
/// Declared here, in the domain, and re-exported by the adapter that owns the
/// channel (`pond_infra::broadcast_notification_sender::BROADCAST_TARGET`) so
/// there is one spelling of it in the tree. Two would be worse than none: the
/// targeted path recognises the sentinel in order to *refuse* it, and a second
/// copy that drifted by one character would turn the refusal off without
/// changing a single line of the code that reads.
///
/// It is reserved rather than merely conventional because a device id is
/// caller-supplied at registration
/// (`sqlite_device_registry::tests::register_honours_caller_supplied_stable_id`).
/// A device registered as `"broadcast"` and then attributed to a member would
/// make a *targeted* delivery to that member fan out to the whole household,
/// which is precisely the failure PAI-7's invariant 4 exists to prevent.
pub const RESERVED_BROADCAST_TARGET: &str = "broadcast";

/// Why a targeted delivery reached nobody.
///
/// Every variant is a **refusal to deliver**, and none of them is a fallback.
/// PAI-7 invariant 4 says a proposal is addressed to a profile and never
/// broadcast, so "I could not work out who to send this to" has exactly one
/// correct consequence and it is silence. Failing to deliver is a reliability
/// failure; delivering to everybody is a privacy failure, and the second is the
/// one this programme is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undeliverable {
    /// The id handed in is not a household member id (blank / whitespace).
    /// [`checked_profile_id`]'s refusal, carried as a plan rather than as an
    /// error, so a caller cannot turn it into a fallback with `unwrap_or`.
    NotAMember,
    /// The attribution read itself failed. **This is the variant the whole type
    /// exists for.** A storage error is the moment a delivery path is most
    /// tempted to "just broadcast so the user still gets it", and on a failed
    /// read access must narrow, not widen.
    AttributionUnavailable(String),
    /// The member has no device of their own. A real, expected state: somebody
    /// who has never paired a phone. Deliberately NOT the same thing as "send it
    /// to the unclaimed kitchen tablet" -- see
    /// [`DeviceAttribution::devices_for_profile`], which never returns one.
    NoAttributedDevice,
    /// Every device attributed to this member is the reserved broadcast
    /// sentinel, so there is no target left that addresses a person.
    ReservedTargetOnly,
}

impl Undeliverable {
    /// Short, stable label for structured logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotAMember => "not_a_member",
            Self::AttributionUnavailable(_) => "attribution_unavailable",
            Self::NoAttributedDevice => "no_attributed_device",
            Self::ReservedTargetOnly => "reserved_target_only",
        }
    }
}

/// Where a targeted notification for one household member actually goes.
///
/// **There is no broadcast variant, and that is the point.** This type is the
/// same move PAI-7 P3a made with `ProposalAudience` and PAI-6 P1 made with
/// `TaskRequest`: the rule that gets written as a runtime check is the rule a
/// later refactor deletes, so invariant 4 is expressed as a shape instead. A
/// delivery path that holds one of these cannot fan out to the household
/// however its author writes the match arms, because there is no arm to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetedDelivery {
    /// Deliver one copy per device id. Non-empty, and never contains
    /// [`RESERVED_BROADCAST_TARGET`].
    ToDevices(Vec<String>),
    /// Deliver to nobody, for this reason.
    Undeliverable(Undeliverable),
}

impl TargetedDelivery {
    /// Turn "who is this for" plus the attribution read into a delivery plan.
    ///
    /// Takes the `Result` rather than the `Vec` on purpose. A signature of
    /// `plan(profile_id, &[String])` puts the failed read back in the caller's
    /// hands, where the shortest thing to write is
    /// `.unwrap_or_default()` -- which answers a storage error with
    /// [`Undeliverable::NoAttributedDevice`] and reads, in a log, exactly like a
    /// member who owns no phone. Consuming the `Result` here makes the
    /// distinction impossible to lose.
    pub fn plan(profile_id: &str, attributed_devices: Result<Vec<String>>) -> Self {
        if checked_profile_id(profile_id).is_err() {
            return Self::Undeliverable(Undeliverable::NotAMember);
        }
        let devices = match attributed_devices {
            Ok(devices) => devices,
            Err(e) => {
                return Self::Undeliverable(Undeliverable::AttributionUnavailable(e.to_string()))
            }
        };
        if devices.is_empty() {
            return Self::Undeliverable(Undeliverable::NoAttributedDevice);
        }
        let addressable: Vec<String> = devices
            .into_iter()
            .filter(|id| id != RESERVED_BROADCAST_TARGET)
            .collect();
        if addressable.is_empty() {
            return Self::Undeliverable(Undeliverable::ReservedTargetOnly);
        }
        Self::ToDevices(addressable)
    }

    /// The device ids to deliver to, empty when this plan delivers to nobody.
    pub fn devices(&self) -> &[String] {
        match self {
            Self::ToDevices(ids) => ids,
            Self::Undeliverable(_) => &[],
        }
    }
}

/// Reject a profile id that is not a member id.
///
/// A blank or whitespace-only id is a caller bug, and both plausible silent
/// answers to it are wrong: writing it stores a value that is neither NULL nor
/// a member -- one `ON DELETE SET NULL` can never clear, because no profile row
/// will ever be deleted to trigger it -- and reading it returns an empty set
/// that is indistinguishable from "this member owns no devices". Refusing is
/// the narrowing answer and the loud one.
///
/// This lives in the port rather than in the adapter because it is the rule,
/// not the mechanism (invariant 5).
pub fn checked_profile_id(profile_id: &str) -> Result<&str> {
    if profile_id.trim().is_empty() {
        return Err(anyhow!(
            "a blank profile id is not a household member; pass the member's id, \
             or None to leave the device unattributed"
        ));
    }
    Ok(profile_id)
}

/// Driven port: the device <-> household-member association.
#[async_trait]
pub trait DeviceAttribution: Send + Sync {
    /// Bind a device to a household member, or release it with `None`.
    ///
    /// Errors if the device is not registered -- a write that silently matched
    /// no row would make a broken wiring look like a successful attribution,
    /// which is how this programme has shipped inert features before.
    async fn set_device_profile(&self, device_id: &str, profile_id: Option<&str>) -> Result<()>;

    /// Which member owns this device. `None` means unattributed: *nobody has
    /// claimed it*, never *everybody*.
    ///
    /// This is the value [`ResolutionInputs::paired_device_profile`] wants.
    ///
    /// [`ResolutionInputs::paired_device_profile`]: crate::user_data::services::identity_resolution::ResolutionInputs::paired_device_profile
    async fn device_profile(&self, device_id: &str) -> Result<Option<String>>;

    /// The ids of the devices attributed to this member, newest registration
    /// first. Never includes an unattributed device.
    ///
    /// Ids rather than `Device` values: the caller that wants a display name
    /// already holds a `DeviceRegistry`, and duplicating the row shape here
    /// would give the hub two ways to read a device that can disagree.
    async fn devices_for_profile(&self, profile_id: &str) -> Result<Vec<String>>;

    /// The push tokens registered by this member's devices.
    ///
    /// The delivery end of PAI-7 section 3.4. An empty vector means this member
    /// has no reachable device, which is a real and expected state -- a member
    /// who has never paired a phone. It is the caller's job to treat that as
    /// "not delivered" rather than falling back to a broadcast.
    async fn push_tokens_for_profile(&self, profile_id: &str) -> Result<Vec<PushToken>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_profile_id_passes_through_unchanged() {
        assert_eq!(checked_profile_id("liz").unwrap(), "liz");
        // Not trimmed: an id with surrounding space is a different id, and
        // silently rewriting it would make the lookup disagree with the write.
        assert_eq!(checked_profile_id(" liz ").unwrap(), " liz ");
    }

    #[test]
    fn a_blank_profile_id_is_refused_rather_than_answered() {
        for blank in ["", " ", "\t", "\n  "] {
            let err = checked_profile_id(blank).unwrap_err().to_string();
            assert!(
                err.contains("blank profile id"),
                "a blank id must be refused by name, got: {err}"
            );
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The positive case, and the vacuity control for every refusal test below:
    /// if `plan` refused everything, they would all pass while proving nothing.
    #[test]
    fn a_member_with_devices_is_delivered_to_each_of_them() {
        let plan = TargetedDelivery::plan("liz", Ok(ids(&["phone-liz", "watch-liz"])));
        assert_eq!(
            plan,
            TargetedDelivery::ToDevices(ids(&["phone-liz", "watch-liz"])),
            "a member with two devices gets two targets, in the order the port returned"
        );
        assert_eq!(plan.devices().len(), 2);
    }

    /// PAI-7 invariant 4, in the direction that costs a privacy failure when it
    /// is wrong. A storage error is where a delivery path is most tempted to
    /// broadcast so that "the user still gets it".
    #[test]
    fn a_failed_attribution_read_delivers_to_nobody_and_says_which_failure_it_was() {
        let plan = TargetedDelivery::plan("liz", Err(anyhow!("database is locked")));
        match &plan {
            TargetedDelivery::Undeliverable(Undeliverable::AttributionUnavailable(why)) => {
                assert!(
                    why.contains("database is locked"),
                    "the storage error must survive into the plan, got: {why}"
                );
            }
            other => panic!(
                "a failed attribution read must be AttributionUnavailable, not {other:?}. \
                 Answering it with an empty device list makes a broken database read, in a log, \
                 exactly like a member who has never paired a phone"
            ),
        }
        assert!(
            plan.devices().is_empty(),
            "nothing is delivered when the read failed"
        );
    }

    /// The port's deliberate asymmetry, carried into the plan: an unattributed
    /// device is nobody's, so a member who owns none is unreachable rather than
    /// reachable through the whole house.
    #[test]
    fn a_member_with_no_attributed_device_is_unreachable_not_broadcast() {
        let plan = TargetedDelivery::plan("liz", Ok(Vec::new()));
        assert_eq!(
            plan,
            TargetedDelivery::Undeliverable(Undeliverable::NoAttributedDevice)
        );
        assert!(plan.devices().is_empty());
    }

    /// A device id is caller-supplied at registration, so `"broadcast"` is a
    /// registrable id. Attributed to a member it would convert a targeted
    /// delivery into a household one at the sender's `target == BROADCAST_TARGET`
    /// branch -- the leak arriving through the front door of the very check
    /// meant to prevent it.
    #[test]
    fn the_reserved_sentinel_is_never_a_delivery_target() {
        let plan =
            TargetedDelivery::plan("liz", Ok(ids(&[RESERVED_BROADCAST_TARGET, "phone-liz"])));
        assert_eq!(
            plan,
            TargetedDelivery::ToDevices(ids(&["phone-liz"])),
            "the sentinel is dropped and the member's real device still gets it"
        );

        let only = TargetedDelivery::plan("liz", Ok(ids(&[RESERVED_BROADCAST_TARGET])));
        assert_eq!(
            only,
            TargetedDelivery::Undeliverable(Undeliverable::ReservedTargetOnly),
            "and when the sentinel is all there is, the answer is nobody -- not everybody"
        );
    }

    #[test]
    fn a_blank_profile_id_produces_a_plan_rather_than_an_error_to_swallow() {
        for blank in ["", "   "] {
            assert_eq!(
                TargetedDelivery::plan(blank, Ok(ids(&["phone-liz"]))),
                TargetedDelivery::Undeliverable(Undeliverable::NotAMember),
                "a blank id must not be allowed to reach a device list that was fetched for \
                 somebody else"
            );
        }
    }

    /// Structural tripwire, and it is a COMPILE-time one rather than an
    /// assertion: this match is exhaustive with no wildcard, so adding a
    /// `TargetedDelivery::Broadcast` (or any other way of expressing "send it to
    /// the household") fails to build here. A runtime assertion could not say
    /// this at all -- the value it would need to construct is the value the type
    /// is supposed to make unconstructible.
    #[test]
    fn the_delivery_plan_cannot_express_a_broadcast() {
        let plan = TargetedDelivery::plan("liz", Ok(ids(&["phone-liz"])));
        let described = match &plan {
            TargetedDelivery::ToDevices(devices) => {
                assert!(
                    !devices.iter().any(|d| d == RESERVED_BROADCAST_TARGET),
                    "ToDevices must never carry the sentinel: {devices:?}"
                );
                "to devices"
            }
            TargetedDelivery::Undeliverable(reason) => match reason {
                Undeliverable::NotAMember
                | Undeliverable::AttributionUnavailable(_)
                | Undeliverable::NoAttributedDevice
                | Undeliverable::ReservedTargetOnly => "to nobody",
            },
        };
        assert_eq!(described, "to devices");
    }

    #[test]
    fn every_undeliverable_reason_has_its_own_log_label() {
        let labels = [
            Undeliverable::NotAMember.as_str(),
            Undeliverable::AttributionUnavailable("x".into()).as_str(),
            Undeliverable::NoAttributedDevice.as_str(),
            Undeliverable::ReservedTargetOnly.as_str(),
        ];
        let unique: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
        assert_eq!(
            unique.len(),
            labels.len(),
            "two reasons sharing a label make the two failures indistinguishable in the one \
             place an operator looks: {labels:?}"
        );
    }
}
