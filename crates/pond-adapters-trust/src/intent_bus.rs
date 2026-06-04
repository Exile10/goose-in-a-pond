//! In-process [`IntentBus`] implementation.
//!
//! State:
//!   * One [`tokio::sync::broadcast::Sender`] per `install_id` — fans out
//!     `BusEvent`s to that device's WebSocket session(s). Replaced on
//!     re-subscribe so a stale receiver doesn't keep events buffered.
//!   * A `HashMap<intent_id → Resolver>` recording outstanding pending
//!     intents. Each `Resolver` is a `oneshot::Sender` the publisher's
//!     `await_resolution` is parked on.
//!
//! Persistence: the authoritative state of pending intents lives in the
//! `pending_intents` SQLite table (see migration 0016). The in-memory map
//! is just a wakeup channel — phones connecting between publish and
//! assertion can still discover the intent by re-querying the DB if we
//! later add that endpoint, without needing the bus to be the SoT.

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use pond_core::domain::trust::{Intent, IntentOutcome, SignedAssertion};
use pond_core::ports::intent_bus::{
    BusEvent, BusReceiver, IntentBus, IntentResolution,
};
use sqlx::{Pool, Sqlite};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, oneshot, Mutex};
use tracing::{debug, info, warn};
use uuid::Uuid;

const FANOUT_BUFFER: usize = 32;

/// One outstanding intent, awaiting resolution.
struct Resolver {
    intent: Intent,
    notify: oneshot::Sender<IntentResolution>,
}

pub struct InMemoryIntentBus {
    pool:        Pool<Sqlite>,
    /// Per-`install_id` broadcast sender. Subscribers `recv()` until
    /// dropped or until the matching intent resolves.
    senders:     Arc<Mutex<HashMap<String, broadcast::Sender<BusEvent>>>>,
    /// Outstanding intents keyed by id.
    pending:     Arc<Mutex<HashMap<Uuid, Resolver>>>,
}

impl InMemoryIntentBus {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self {
            pool,
            senders: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// List paired devices. We fan out new intents to every install_id
    /// that has a registered public key, regardless of whether it's
    /// currently subscribed — late-joining devices can pull from the DB.
    async fn target_install_ids(&self) -> Vec<String> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT install_id FROM device_pubkeys WHERE revoked_at IS NULL",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        rows.into_iter().map(|(id,)| id).collect()
    }

    async fn record_pending(&self, intent: &Intent) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO pending_intents \
                 (id, action, summary, payload_hash, requested_by, created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(intent.id.to_string())
        .bind(&intent.action)
        .bind(&intent.summary)
        .bind(intent.payload_hash.to_vec())
        .bind(&intent.requested_by)
        .bind(intent.created_at.to_rfc3339())
        .bind(intent.expires_at.to_rfc3339())
        .execute(&self.pool)
        .await
        .context("pending_intents insert failed")?;
        Ok(())
    }

    async fn resolve_pending(
        &self,
        intent_id: Uuid,
        outcome:   IntentOutcome,
        signer:    Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE pending_intents SET resolved_at = ?, outcome = ?, resolved_by = ? \
             WHERE id = ? AND resolved_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(match outcome {
            IntentOutcome::Approved => "approved",
            IntentOutcome::Denied => "denied",
            IntentOutcome::Expired => "expired",
        })
        .bind(signer)
        .bind(intent_id.to_string())
        .execute(&self.pool)
        .await
        .context("pending_intents resolve failed")?;
        Ok(())
    }
}

#[async_trait]
impl IntentBus for InMemoryIntentBus {
    async fn publish(&self, intent: Intent) -> anyhow::Result<()> {
        self.record_pending(&intent).await?;

        let install_ids = self.target_install_ids().await;
        if install_ids.is_empty() {
            warn!(intent_id = %intent.id, "no paired devices — intent will time out");
        }

        let senders = self.senders.lock().await;
        for id in &install_ids {
            if let Some(tx) = senders.get(id) {
                // Best-effort send — receiver may be lagging or absent.
                let _ = tx.send(BusEvent::Intent(intent.clone()));
            }
        }
        info!(
            intent_id = %intent.id,
            action = %intent.action,
            fanout = install_ids.len(),
            "intent published"
        );
        Ok(())
    }

    async fn subscribe(
        &self,
        install_id: &str,
    ) -> anyhow::Result<Box<dyn BusReceiver>> {
        let mut senders = self.senders.lock().await;
        let tx = senders
            .entry(install_id.to_string())
            .or_insert_with(|| broadcast::channel(FANOUT_BUFFER).0);
        let rx = tx.subscribe();
        Ok(Box::new(BroadcastReceiver { rx }))
    }

    async fn submit_assertion(
        &self,
        install_id: &str,
        assertion:  SignedAssertion,
    ) -> anyhow::Result<()> {
        let mut pending = self.pending.lock().await;
        let Some(resolver) = pending.remove(&assertion.intent_id) else {
            debug!(install_id, intent_id = %assertion.intent_id, "assertion for unknown / resolved intent — dropping");
            return Ok(());
        };
        // Fan out a Cancel so other devices stop showing the prompt.
        let cancel_id = resolver.intent.id;
        let senders = self.senders.lock().await;
        for tx in senders.values() {
            let _ = tx.send(BusEvent::Cancel(cancel_id));
        }
        drop(senders);

        let _ = resolver.notify.send(IntentResolution::Approved {
            install_id: install_id.to_string(),
            assertion,
        });
        let _ = self
            .resolve_pending(cancel_id, IntentOutcome::Approved, Some(install_id))
            .await;
        Ok(())
    }

    async fn submit_denial(
        &self,
        install_id: &str,
        intent_id:  Uuid,
    ) -> anyhow::Result<()> {
        let mut pending = self.pending.lock().await;
        let Some(resolver) = pending.remove(&intent_id) else { return Ok(()); };
        let cancel_id = resolver.intent.id;
        let senders = self.senders.lock().await;
        for tx in senders.values() {
            let _ = tx.send(BusEvent::Cancel(cancel_id));
        }
        drop(senders);
        let _ = resolver.notify.send(IntentResolution::Denied {
            outcome: IntentOutcome::Denied,
        });
        let _ = self
            .resolve_pending(cancel_id, IntentOutcome::Denied, Some(install_id))
            .await;
        Ok(())
    }

    async fn await_resolution(
        &self,
        intent_id: Uuid,
        timeout:   Duration,
    ) -> anyhow::Result<IntentResolution> {
        // The intent must have been published and recorded already by the
        // caller. We need an Intent struct for the Resolver, so pull it
        // from the pending_intents row we just wrote.
        let row: Option<(String, String, Vec<u8>, String, String, String)> = sqlx::query_as(
            "SELECT action, summary, payload_hash, requested_by, created_at, expires_at \
             FROM pending_intents WHERE id = ?",
        )
        .bind(intent_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .context("pending_intents lookup failed")?;
        let (action, summary, payload_hash_blob, requested_by, created_at_s, expires_at_s) =
            row.ok_or_else(|| anyhow::anyhow!("intent {} not found in pending_intents", intent_id))?;
        let payload_hash: [u8; 32] = payload_hash_blob
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("payload_hash length mismatch in DB"))?;
        let intent = Intent {
            id: intent_id,
            action,
            summary,
            payload_hash,
            requested_by,
            created_at: chrono::DateTime::parse_from_rfc3339(&created_at_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            expires_at: chrono::DateTime::parse_from_rfc3339(&expires_at_s)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        };

        let (notify_tx, notify_rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(
                intent_id,
                Resolver {
                    intent,
                    notify: notify_tx,
                },
            );
        }

        let outcome = match tokio::time::timeout(timeout, notify_rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_canceller_dropped)) => IntentResolution::Denied {
                outcome: IntentOutcome::Expired,
            },
            Err(_timed_out) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&intent_id);
                let _ = self
                    .resolve_pending(intent_id, IntentOutcome::Expired, None)
                    .await;
                // Cancel any phones still showing the prompt.
                let senders = self.senders.lock().await;
                for tx in senders.values() {
                    let _ = tx.send(BusEvent::Cancel(intent_id));
                }
                IntentResolution::Denied {
                    outcome: IntentOutcome::Expired,
                }
            }
        };
        Ok(outcome)
    }
}

struct BroadcastReceiver {
    rx: broadcast::Receiver<BusEvent>,
}

#[async_trait]
impl BusReceiver for BroadcastReceiver {
    async fn recv(&mut self) -> Option<BusEvent> {
        loop {
            match self.rx.recv().await {
                Ok(ev) => return Some(ev),
                // Lagged: skip and continue. Phones don't care about missed
                // intents that already expired.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}
