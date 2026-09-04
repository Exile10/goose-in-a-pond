//! Proof-of-settlement record exchanged by `PaymentRail` and `UsageTally` (#132).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::millisats::Millisats;
use super::peer_id::PeerId;

/// Millisats owed per borrowed token: the rate every Pond settles at.
///
/// Baked in, not negotiated and not per-Pond configurable — a borrower that could set
/// its own rate could decide to pay less. Directional, not business-signed-off yet.
pub const MESH_SETTLEMENT_MILLISATS_PER_TOKEN: u64 = 30;

/// A completed off-hot-path Lightning settlement with a trusted peer.
///
/// The `preimage` is the Lightning payment proof — acceptance criteria for
/// #132 require it be retained as evidence the batched settlement occurred.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettlementRecord {
    pub peer_id: PeerId,
    pub amount: Millisats,
    pub preimage: String,
    pub settled_at: DateTime<Utc>,
}
