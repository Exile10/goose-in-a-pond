//! Proof-of-settlement record exchanged by `PaymentRail` and `UsageTally` (#132).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::millisats::Millisats;
use super::peer_id::PeerId;

/// The exchange rate every Pond settles at: millisats owed per borrowed
/// token.
///
/// A dev-decided constant, not a per-Pond setting and not something the two
/// sides of a borrow negotiate — a borrower that could set its own rate
/// could simply decide to pay less (or nothing) regardless of what the
/// lender expects. One number, baked into the software the same way on
/// every install, is what makes "circle" trust meaningful for payment: your
/// peer already trusts you enough to run their model; they don't also have
/// to trust that your local settings weren't tampered with.
///
/// Not yet a final number — flagged internally as directional, not
/// business-signed-off.
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
