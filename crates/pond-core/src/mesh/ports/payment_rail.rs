use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::millisats::Millisats;
use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::domain::settlement::SettlementRecord;

#[derive(Error, Debug)]
pub enum PaymentRailError {
    #[error("Invalid invoice: {0}")]
    InvalidInvoice(String),

    #[error("Settlement failed: {0}")]
    SettlementFailed(String),
}

/// Driven Port: PaymentRail
///
/// Lightning settlement, off the inference hot path by construction: only a background
/// settlement job calls `batch_settle`, once per threshold or interval, never per token.
#[async_trait]
pub trait PaymentRail: Send + Sync {
    async fn issue_invoice(&self, amount: Millisats) -> Result<String, PaymentRailError>;

    async fn verify_preimage(
        &self,
        invoice: &str,
        preimage: &str,
    ) -> Result<bool, PaymentRailError>;

    /// Pay `peer` against *their* `invoice`, obtained by the caller beforehand (e.g. via
    /// `MeshInferenceService::request_invoice`) since this port has no transport of its
    /// own. `amount` is passed separately from the amount encoded in the opaque,
    /// BOLT11-shaped `invoice` so an adapter can cross-check the two before paying.
    async fn batch_settle(
        &self,
        peer: PeerId,
        amount: Millisats,
        invoice: &str,
    ) -> Result<SettlementRecord, PaymentRailError>;
}
