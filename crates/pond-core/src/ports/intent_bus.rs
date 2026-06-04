//! IntentBus port — the pond's mechanism for asking a paired phone to
//! authorise a Privileged action.
//!
//! Lifecycle of one intent:
//!
//! ```text
//!   handler creates Intent ──▶ bus.publish(intent)
//!                                    │
//!                                    │ broadcast over WS to all paired devices
//!                                    ▼
//!                              GOTG receives, prompts Face ID
//!                                    │
//!                                    │ phone signs scope, sends back over WS
//!                                    ▼
//!                              bus.submit_assertion(install_id, assertion)
//!                                    │
//!                                    ▼
//!   handler awaits  ──────▶  bus.await_assertion(intent_id, timeout)
//!                                    │
//!                                    ▼
//!                              returns Some(assertion) or None on timeout
//! ```
//!
//! The bus is process-local; it doesn't persist intents because they are
//! short-lived (60 s) and their authoritative state lives in the
//! `pending_intents` SQLite table.

use crate::domain::trust::{Intent, IntentOutcome, SignedAssertion};
use anyhow::Result;
use async_trait::async_trait;
use std::time::Duration;
use uuid::Uuid;

/// Subscriber stream item: either a new intent fanned out to this device,
/// or a cancellation (the intent expired or another device approved it).
#[derive(Debug, Clone)]
pub enum BusEvent {
    Intent(Intent),
    Cancel(Uuid),
}

/// Driven port: pub/sub channel from privileged-tier handlers to
/// connected GOTG WebSocket sessions.
#[async_trait]
pub trait IntentBus: Send + Sync {
    /// Publish a new intent. Fans out to every active subscriber. Persists
    /// the intent into `pending_intents` so a phone connecting between
    /// publish and assertion can still discover it.
    async fn publish(&self, intent: Intent) -> Result<()>;

    /// Subscribe to bus events for `install_id`. Returns a boxed receiver
    /// the caller drains for the lifetime of the WS connection.
    async fn subscribe(
        &self,
        install_id: &str,
    ) -> Result<Box<dyn BusReceiver>>;

    /// Phone-side: deliver a signed assertion back to the bus. The bus
    /// matches it to an outstanding intent and unblocks the handler that
    /// is awaiting it. Idempotent — repeats are ignored.
    async fn submit_assertion(
        &self,
        install_id: &str,
        assertion:  SignedAssertion,
    ) -> Result<()>;

    /// Phone-side: explicit denial. Resolves the intent immediately so the
    /// handler returns 403 instead of waiting for the timeout.
    async fn submit_denial(&self, install_id: &str, intent_id: Uuid) -> Result<()>;

    /// Handler-side: block until any device responds for `intent_id`.
    /// Returns the assertion on approval, or [`IntentOutcome::Denied`] /
    /// [`IntentOutcome::Expired`] otherwise.
    async fn await_resolution(
        &self,
        intent_id: Uuid,
        timeout:   Duration,
    ) -> Result<IntentResolution>;
}

/// Outcome of an `await_resolution` call.
#[derive(Debug)]
pub enum IntentResolution {
    Approved {
        install_id: String,
        assertion:  SignedAssertion,
    },
    Denied {
        outcome: IntentOutcome,
    },
}

/// Async iterator handle returned by [`IntentBus::subscribe`]. Kept as a
/// trait object so the port stays object-safe.
#[async_trait]
pub trait BusReceiver: Send {
    async fn recv(&mut self) -> Option<BusEvent>;
}
