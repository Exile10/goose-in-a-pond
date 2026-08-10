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
}
