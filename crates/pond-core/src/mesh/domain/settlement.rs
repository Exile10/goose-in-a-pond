//! Proof-of-settlement record exchanged by `PaymentRail` and `UsageTally` (#132).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::millisats::Millisats;
use super::peer_id::PeerId;

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
