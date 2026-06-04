//! Source-of-truth table mapping canonical action names to required trust
//! levels. New endpoints classified as Privileged or Personal MUST be
//! registered here; anything not present defaults to Ambient.
//!
//! Action names use a dotted convention `domain.verb_object` (e.g.
//! `settings.update_wake_word`). They are part of the signature scope, so
//! renaming an action here is a *breaking* change for already-issued
//! biometric assertions — bump the protocol version (`giap-intent-v2`) if
//! that ever needs to happen.

use pond_core::domain::trust::{RemotePolicy, TrustLevel};

/// One row in the trust table.
#[derive(Debug, Clone, Copy)]
pub struct TrustEntry {
    pub action:        &'static str,
    pub level:         TrustLevel,
    pub remote_policy: RemotePolicy,
}

/// The canonical table. Lookups are O(N) on a small N; for N > ~50 we'd
/// switch to a phf::Map but the table is shorter than the route list and
/// pruning happens at registration time anyway.
pub const TRUST_TABLE: &[TrustEntry] = &[
    // ── Privileged: irreversible / high-risk ────────────────────────────
    TrustEntry {
        action:        "settings.update_wake_word",
        level:         TrustLevel::Privileged,
        remote_policy: RemotePolicy::Allowed,
    },
    TrustEntry {
        action:        "settings.update_voice_provider",
        level:         TrustLevel::Privileged,
        remote_policy: RemotePolicy::Allowed,
    },
    TrustEntry {
        action:        "settings.update_chat_provider",
        level:         TrustLevel::Privileged,
        remote_policy: RemotePolicy::Allowed,
    },
    TrustEntry {
        action:        "face.delete_biometrics",
        level:         TrustLevel::Privileged,
        remote_policy: RemotePolicy::LocalOnly,
    },
    TrustEntry {
        action:        "face.register",
        level:         TrustLevel::Privileged,
        remote_policy: RemotePolicy::LocalOnly,
    },
    TrustEntry {
        action:        "memory.delete_all",
        level:         TrustLevel::Privileged,
        remote_policy: RemotePolicy::LocalOnly,
    },
    TrustEntry {
        action:        "device.revoke_pairing",
        level:         TrustLevel::Privileged,
        remote_policy: RemotePolicy::Allowed,
    },
    // ── Personal: user-scoped reads/writes ──────────────────────────────
    TrustEntry {
        action:        "calendar.list_events",
        level:         TrustLevel::Personal,
        remote_policy: RemotePolicy::Allowed,
    },
    TrustEntry {
        action:        "memory.read",
        level:         TrustLevel::Personal,
        remote_policy: RemotePolicy::Allowed,
    },
    TrustEntry {
        action:        "settings.read",
        level:         TrustLevel::Personal,
        remote_policy: RemotePolicy::Allowed,
    },
    // ── Ambient defaults are implied by absence; keeping a few here for
    //    documentation purposes and so they show up in any future
    //    rendered trust-table page. ─────────────────────────────────────
    TrustEntry {
        action:        "weather.current",
        level:         TrustLevel::Ambient,
        remote_policy: RemotePolicy::Allowed,
    },
];

/// Lookup `action` in the table. Returns `Ambient/Allowed` when not found
/// — the safe default for an unclassified read endpoint.
pub fn classify(action: &str) -> TrustEntry {
    for entry in TRUST_TABLE {
        if entry.action == action {
            return *entry;
        }
    }
    TrustEntry {
        action:        "unknown",
        level:         TrustLevel::Ambient,
        remote_policy: RemotePolicy::Allowed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_action_returns_classification() {
        let e = classify("face.delete_biometrics");
        assert_eq!(e.level, TrustLevel::Privileged);
        assert_eq!(e.remote_policy, RemotePolicy::LocalOnly);
    }

    #[test]
    fn unknown_action_defaults_to_ambient() {
        let e = classify("nonsense.action");
        assert_eq!(e.level, TrustLevel::Ambient);
        assert_eq!(e.remote_policy, RemotePolicy::Allowed);
    }

    #[test]
    fn no_duplicate_action_names() {
        let mut names: Vec<&str> = TRUST_TABLE.iter().map(|e| e.action).collect();
        names.sort();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(
            names, deduped,
            "duplicate action name in TRUST_TABLE — would cause silent shadowing"
        );
    }
}
