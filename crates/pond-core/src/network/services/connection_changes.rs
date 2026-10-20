//! Who may change the pond's network links (`pond-connections.md`, "Decisions"). Reading them
//! takes any paired token; changing them can cut every phone off the pond, so it takes more.

use crate::security::domain::proven_device::DeviceRung;

/// The wire code every refused change answers with.
pub const NOT_ALLOWED: &str = "not_allowed";

/// Why a caller may not change the pond's network links. Each says who the caller is, never what
/// the OS allows: that is the station's capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChangeRefusal {
    #[error(
        "Only the phone that set up this pond, or a household member's device, can change its network"
    )]
    NotAMember,
    #[error(
        "This request did not come from a paired device, so it cannot change the pond's network"
    )]
    NoDevice,
    #[error(
        "The pond could not read whose device this is, so it cannot let it change the network; try again"
    )]
    AttributionUnreadable,
}

impl ChangeRefusal {
    /// Short, stable label for structured logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAMember => "not_a_member",
            Self::NoDevice => "no_device",
            Self::AttributionUnreadable => "attribution_unreadable",
        }
    }
}

/// The host credential, the device that claimed the pond, or a device attributed to a household
/// member. The claimant counts before onboarding attributes it, so owner setup can fix the network;
/// an unattributed device (a guest's phone) may look but not change.
pub fn may_change_connections(
    host: bool,
    device: Option<&str>,
    claimant: Option<&str>,
    attribution: &DeviceRung,
) -> Result<(), ChangeRefusal> {
    if host {
        return Ok(());
    }
    let Some(device) = device.filter(|device| !device.is_empty()) else {
        return Err(ChangeRefusal::NoDevice);
    };
    if claimant == Some(device) {
        return Ok(());
    }
    match attribution {
        DeviceRung::Member(_) => Ok(()),
        DeviceRung::Unattributed => Err(ChangeRefusal::NotAMember),
        DeviceRung::NoDevice => Err(ChangeRefusal::NoDevice),
        // A failed read narrows: it never assumes the device is a member's.
        DeviceRung::Unavailable(_) => Err(ChangeRefusal::AttributionUnreadable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE: Option<&str> = Some("install-amina");

    fn member() -> DeviceRung {
        DeviceRung::Member("amina".into())
    }

    #[test]
    fn the_host_may_change_whoever_else_is_asking() {
        for rung in [
            DeviceRung::NoDevice,
            DeviceRung::Unattributed,
            DeviceRung::Unavailable("database is locked".into()),
        ] {
            assert_eq!(may_change_connections(true, None, None, &rung), Ok(()));
            assert_eq!(may_change_connections(true, PHONE, None, &rung), Ok(()));
        }
    }

    #[test]
    fn the_claimant_may_change_before_any_member_is_attributed() {
        assert_eq!(
            may_change_connections(false, PHONE, PHONE, &DeviceRung::Unattributed),
            Ok(())
        );
        assert_eq!(
            may_change_connections(
                false,
                PHONE,
                PHONE,
                &DeviceRung::Unavailable("disk gone".into())
            ),
            Ok(()),
            "the claim is proof enough without the attribution read"
        );
    }

    #[test]
    fn a_members_device_may_change() {
        assert_eq!(
            may_change_connections(false, PHONE, Some("install-other"), &member()),
            Ok(())
        );
        assert_eq!(
            may_change_connections(false, PHONE, None, &member()),
            Ok(())
        );
    }

    #[test]
    fn an_unattributed_device_that_did_not_claim_may_only_look() {
        assert_eq!(
            may_change_connections(
                false,
                PHONE,
                Some("install-owner"),
                &DeviceRung::Unattributed
            ),
            Err(ChangeRefusal::NotAMember)
        );
        assert_eq!(
            may_change_connections(false, PHONE, None, &DeviceRung::Unattributed),
            Err(ChangeRefusal::NotAMember),
            "a pond nobody claimed is not an open door"
        );
    }

    #[test]
    fn no_device_or_an_unreadable_attribution_narrows() {
        assert_eq!(
            may_change_connections(false, None, PHONE, &member()),
            Err(ChangeRefusal::NoDevice),
            "a member rung with no device on the request speaks for nobody"
        );
        assert_eq!(
            may_change_connections(false, Some(""), Some(""), &DeviceRung::Unattributed),
            Err(ChangeRefusal::NoDevice),
            "a blank id never matches a blank claimant"
        );
        assert_eq!(
            may_change_connections(false, PHONE, None, &DeviceRung::NoDevice),
            Err(ChangeRefusal::NoDevice)
        );
        assert_eq!(
            may_change_connections(
                false,
                PHONE,
                Some("install-owner"),
                &DeviceRung::Unavailable("database is locked".into())
            ),
            Err(ChangeRefusal::AttributionUnreadable)
        );
    }

    #[test]
    fn each_refusal_says_who_the_caller_is_and_has_its_own_label() {
        let refusals = [
            ChangeRefusal::NotAMember,
            ChangeRefusal::NoDevice,
            ChangeRefusal::AttributionUnreadable,
        ];
        let labels: std::collections::BTreeSet<&str> =
            refusals.iter().map(|r| r.as_str()).collect();
        assert_eq!(labels.len(), refusals.len());
        assert_eq!(
            ChangeRefusal::NotAMember.to_string(),
            "Only the phone that set up this pond, or a household member's device, can change its \
             network"
        );
        assert_eq!(NOT_ALLOWED, "not_allowed");
    }
}
