//! The mesh's trust dial (#132).
//!
//! v1 ships `SelfOwned` and `Circle` only; #132 defers the `OpenLane` (public,
//! zero-trust) capability, so add that variant when the capability is built.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustScope {
    /// A peer running on hardware the same owner controls.
    SelfOwned,
    /// A peer belonging to a trusted family/friends/community circle.
    Circle,
}
