use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::millisats::Millisats;
use crate::mesh::domain::peer_id::PeerId;

#[derive(Error, Debug)]
pub enum InvoiceRequesterError {
    #[error("mesh transport error: {0}")]
    Transport(String),

    #[error("peer {0} reported an error: {1}")]
    PeerError(PeerId, String),

    #[error("no invoice response from peer {0} before timeout")]
    Timeout(PeerId),
}

/// Driven Port: InvoiceRequester
///
/// Ask a trusted peer for an invoice covering `amount` — the mesh-transport
/// half of settlement. `PaymentRail::batch_settle` needs the peer's real
/// invoice string before it can pay them, and this crate has no transport
/// of its own to ask for one; this port is that ask, carried over the mesh.
/// Implemented by `pond_adapters_mesh_inference::MeshInferenceService` (it
/// already owns the mesh transport's sole `recv()` consumer — see that
/// crate's own docs on why every mesh-facing capability has to go through
/// it rather than opening a second, competing consumer).
#[async_trait]
pub trait InvoiceRequester: Send + Sync {
    async fn request_invoice(
        &self,
        peer: PeerId,
        amount: Millisats,
    ) -> Result<String, InvoiceRequesterError>;
}
