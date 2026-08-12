//! The mesh's trust dial (#132).
//!
//! v1 ships `SelfOwned` and `Circle` only — the issue explicitly defers the
//! `OpenLane` (public, zero-trust) capability, so it is not represented here.
//! Add it when that capability is actually built rather than carrying an
//! unused variant through every match arm until then.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustScope {
    /// A peer running on hardware the same owner controls.
    SelfOwned,
    /// A peer belonging to a trusted family/friends/community circle.
    Circle,
}
