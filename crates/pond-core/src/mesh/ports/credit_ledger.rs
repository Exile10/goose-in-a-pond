use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::millisats::Millisats;
use crate::mesh::domain::peer_id::PeerId;

#[derive(Error, Debug)]
pub enum CreditLedgerError {
    #[error("Insufficient balance with peer {peer}: have {have}, need {need}")]
    InsufficientBalance {
        peer: PeerId,
        have: Millisats,
        need: Millisats,
    },

    #[error("Ledger error: {0}")]
    General(String),
}

/// Driven Port: CreditLedger
///
/// The prepaid-credit balance held with each trusted peer. `debit` is the
/// hot-path call (balance check before serving/borrowing a request) and must
/// never go negative — a debit past the available balance is a typed error,
/// never a silent wraparound or panic.
#[async_trait]
pub trait CreditLedger: Send + Sync {
    async fn balance(&self, peer: PeerId) -> Result<Millisats, CreditLedgerError>;

    async fn credit(&self, peer: PeerId, amount: Millisats) -> Result<(), CreditLedgerError>;

    async fn debit(&self, peer: PeerId, amount: Millisats) -> Result<(), CreditLedgerError>;
}
