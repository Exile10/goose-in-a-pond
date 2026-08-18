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
/// Lightning settlement, off the inference hot path by construction: nothing
/// in the request/response path calls this port directly — a background
/// settlement job calls `batch_settle` once per threshold/interval, never
/// per token. The invoice string is opaque here (BOLT11-shaped, but this
/// layer doesn't parse it) — that's the concrete adapter's concern.
#[async_trait]
pub trait PaymentRail: Send + Sync {
    async fn issue_invoice(&self, amount: Millisats) -> Result<String, PaymentRailError>;

    async fn verify_preimage(
        &self,
        invoice: &str,
        preimage: &str,
    ) -> Result<bool, PaymentRailError>;

    /// Pay `peer` `amount` against `invoice` — *their* invoice, obtained by
    /// the caller beforehand (e.g. via
    /// `pond_adapters_mesh_inference::MeshInferenceService::request_invoice`,
    /// the mesh-side half of invoice exchange; this port has no transport of
    /// its own to ask for one). `amount` is passed separately from the
    /// amount encoded in `invoice` so an adapter can cross-check the two
    /// don't disagree before paying.
    async fn batch_settle(
        &self,
        peer: PeerId,
        amount: Millisats,
        invoice: &str,
    ) -> Result<SettlementRecord, PaymentRailError>;
}
