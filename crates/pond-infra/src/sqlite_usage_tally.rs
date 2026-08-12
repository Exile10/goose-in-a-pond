//! SQLite-backed implementation of the UsageTally port (#132 Milestone 4).
//!
//! Wraps `Pool<Sqlite>` pointing at `pond_system.db`. Table created by
//! `migrations/system/0046_mesh_ledger.sql`.

use async_trait::async_trait;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::domain::token_count::TokenCount;
use pond_core::mesh::ports::usage_tally::{UsageTally, UsageTallyError};
use sqlx::{Pool, Sqlite};

pub struct SqliteUsageTally {
    pool: Pool<Sqlite>,
}

impl SqliteUsageTally {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UsageTally for SqliteUsageTally {
    async fn record_usage(&self, peer: PeerId, tokens: TokenCount) -> Result<(), UsageTallyError> {
        sqlx::query(
            "INSERT INTO mesh_usage_tally (peer_id, pending_tokens, updated_at) \
             VALUES (?, ?, datetime('now')) \
             ON CONFLICT(peer_id) DO UPDATE SET \
                pending_tokens = pending_tokens + excluded.pending_tokens, \
                updated_at = datetime('now')",
        )
        .bind(peer.to_string())
        .bind(tokens.value() as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| UsageTallyError::General(e.to_string()))?;
        Ok(())
    }

    async fn pending_tally(&self, peer: PeerId) -> Result<TokenCount, UsageTallyError> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT pending_tokens FROM mesh_usage_tally WHERE peer_id = ?")
                .bind(peer.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| UsageTallyError::General(e.to_string()))?;
        Ok(TokenCount::new(row.map(|(t,)| t as u64).unwrap_or(0)))
    }

    async fn mark_settled(&self, peer: PeerId, up_to: TokenCount) -> Result<(), UsageTallyError> {
        // Atomic conditional decrement — see SqliteCreditLedger::debit for why
        // this is safe under concurrent callers without a transaction/lock.
        let result = sqlx::query(
            "UPDATE mesh_usage_tally \
             SET pending_tokens = pending_tokens - ?, updated_at = datetime('now') \
             WHERE peer_id = ? AND pending_tokens >= ?",
        )
        .bind(up_to.value() as i64)
        .bind(peer.to_string())
        .bind(up_to.value() as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| UsageTallyError::General(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(UsageTallyError::General(format!(
                "settled more than pending for peer {peer}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::sync::Arc;
    use tempfile::tempdir;

    async fn make_tally() -> (SqliteUsageTally, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (SqliteUsageTally::new(db.system), tmp)
    }

    #[tokio::test]
    async fn trait_object_conformance() {
        let (tally, _tmp) = make_tally().await;
        let _: Arc<dyn UsageTally> = Arc::new(tally);
    }

    #[tokio::test]
    async fn record_usage_accumulates() {
        let (tally, _tmp) = make_tally().await;
        let peer = PeerId::from([1u8; 32]);
        tally
            .record_usage(peer, TokenCount::new(100))
            .await
            .unwrap();
        tally.record_usage(peer, TokenCount::new(50)).await.unwrap();
        assert_eq!(
            tally.pending_tally(peer).await.unwrap(),
            TokenCount::new(150)
        );
    }

    #[tokio::test]
    async fn mark_settled_reduces_pending() {
        let (tally, _tmp) = make_tally().await;
        let peer = PeerId::from([2u8; 32]);
        tally
            .record_usage(peer, TokenCount::new(100))
            .await
            .unwrap();
        tally.mark_settled(peer, TokenCount::new(60)).await.unwrap();
        assert_eq!(
            tally.pending_tally(peer).await.unwrap(),
            TokenCount::new(40)
        );
    }

    #[tokio::test]
    async fn mark_settled_more_than_pending_errors() {
        let (tally, _tmp) = make_tally().await;
        let peer = PeerId::from([3u8; 32]);
        tally.record_usage(peer, TokenCount::new(10)).await.unwrap();
        let result = tally.mark_settled(peer, TokenCount::new(11)).await;
        assert!(result.is_err());
        // Pending is unchanged after a failed settle.
        assert_eq!(
            tally.pending_tally(peer).await.unwrap(),
            TokenCount::new(10)
        );
    }

    #[tokio::test]
    async fn unknown_peer_has_zero_pending() {
        let (tally, _tmp) = make_tally().await;
        let peer = PeerId::from([4u8; 32]);
        assert_eq!(tally.pending_tally(peer).await.unwrap(), TokenCount::new(0));
    }
}
