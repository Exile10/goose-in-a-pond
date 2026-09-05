//! Attributing a thing to a household member when nobody said which one.
//!
//! `Handshake::issue_pairing_code_for` has taken an `Option<&str>` since PAI-1
//! P9, and the pairing adapter has always honoured it: a member-bound code
//! makes the device theirs, an ordinary one leaves it unattributed. The port's
//! own doc records the other half — `None` is "the default and the only value
//! any shipped caller produces today".
//!
//! So the attributed path was complete and nothing drove it. Measured on a real
//! pond: twelve devices, none attributed, and therefore every turn falling
//! through the paired-device rung to `Household`. That is the reader-with-no-
//! writer shape, and it costs more than it looks: `sessions.profile_id` is only
//! ever written by the identification chain, so an unattributed pond can never
//! produce a retrievable conversation summary — the summary corpus is refused
//! at the index for want of an owner.
//!
//! This is the writer. It answers one question and does not reach for a
//! database to do it.
//!
//! Two callers now: a pairing code choosing which member a device becomes, and
//! the context-source route choosing which member an account belongs to. They
//! ask the same question, so they share [`sole_member`] rather than each
//! growing their own "if there is only one profile" branch — which is how two
//! surfaces end up disagreeing about who somebody is.
//!
//! # Why a sole member is a safe default and a second one is not
//!
//! Pairing is a deliberate physical act: an operator standing at the pond reads
//! a code and types it into a device. In a one-member household there is
//! exactly one answer to "whose device is this", and refusing to write it down
//! does not make the pond safer — it makes it forgetful.
//!
//! With two or more members there is no such answer, and guessing one would
//! attribute a phone to whoever happened to be created first. So this narrows
//! to `None` and the operator names the member.
//!
//! Deliberately NOT the same rule as [`identity_resolution`], which refuses to
//! resolve a sole member as `Owner` for an unidentified TURN. The difference is
//! real: a turn arrives from nobody in particular, while a pairing code is
//! handed to a device on purpose. Conflating them would let an anonymous turn
//! inherit an identity nobody claimed.
//!
//! [`identity_resolution`]: super::identity_resolution

/// Decide the member a new pairing code should bind to.
///
/// * `explicit` — a member the operator named. Always wins; this function never
///   overrides an operator.
/// * `unattributed_requested` — the operator asked for a code that binds to
///   nobody. Honoured even in a one-member household, because that is how a
///   guest's phone gets paired without becoming the member's.
/// * `member_ids` — every household member. Exactly one is the only shape that
///   produces a default.
///
/// Returns the member id to bind, or `None` for an unattributed code.
pub fn owner_for_new_code(
    explicit: Option<&str>,
    unattributed_requested: bool,
    member_ids: &[String],
) -> Option<String> {
    if let Some(named) = explicit {
        return Some(named.to_string());
    }
    if unattributed_requested {
        return None;
    }
    sole_member(member_ids)
}

/// The one member, when there is exactly one.
///
/// The whole household-size rule, in one place. Zero members has no answer and
/// two has no answer either — with two, picking one would attribute by creation
/// order, which is evidence of nothing.
pub fn sole_member(member_ids: &[String]) -> Option<String> {
    match member_ids {
        [only] => Some(only.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_named_member_always_wins() {
        assert_eq!(
            owner_for_new_code(Some("liz"), false, &ids(&["jerry", "liz"])),
            Some("liz".into())
        );
    }

    /// An operator who named a member and also asked for unattributed is
    /// contradicting themselves; the NAME is the specific instruction, so it
    /// wins. Narrowing here would silently discard what they typed.
    #[test]
    fn a_named_member_outranks_an_unattributed_request() {
        assert_eq!(
            owner_for_new_code(Some("liz"), true, &ids(&["jerry", "liz"])),
            Some("liz".into())
        );
    }

    /// The defect this function exists to fix.
    #[test]
    fn a_sole_member_gets_the_device() {
        assert_eq!(
            owner_for_new_code(None, false, &ids(&["jerry"])),
            Some("jerry".into())
        );
    }

    /// The escape hatch. Without it a one-member pond could not pair a guest's
    /// phone without that phone becoming the member's, and losing that is a
    /// worse trade than the convenience is worth.
    #[test]
    fn a_sole_member_household_can_still_ask_for_nobody() {
        assert_eq!(owner_for_new_code(None, true, &ids(&["jerry"])), None);
    }

    /// Two members is no answer, not a coin flip. Binding to the first would
    /// attribute a phone by creation order, which is not evidence of anything.
    #[test]
    fn two_members_is_not_a_default() {
        assert_eq!(
            owner_for_new_code(None, false, &ids(&["jerry", "liz"])),
            None
        );
    }

    /// A pond with no profiles at all pairs unattributed rather than failing:
    /// the device is still usable, it simply belongs to nobody yet.
    #[test]
    fn an_empty_household_pairs_unattributed() {
        assert_eq!(owner_for_new_code(None, false, &[]), None);
    }
}
