//! `NotificationSender` over an in-process broadcast channel + offline queue (#99).
//!
//! - foreground: publish on a `tokio::sync::broadcast` channel that connected
//!   `/notifications/stream` clients subscribe to;
//! - offline: persist targeted notifications to the queue so a disconnected
//!   device gets them on reconnect;
//! - background (scaffold): hand targeted notifications to the optional
//!   [`NotificationRelay`] (FCM/APNs), best-effort.
//!
//! SCAFFOLD STATUS (#99): the queue + relay paths are exercised only via
//! *targeted* `send()`. Every current production producer publishes with
//! `broadcast()` (the ephemeral foreground path), so the offline queue and relay
//! stay dormant until a real targeted producer is wired for phase-3 push. The
//! machinery is complete and unit-tested; only the producer call site is pending.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification::{Notification, NotificationSender};
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use pond_core::mcp::ports::notification_relay::NotificationRelay;
use tokio::sync::broadcast;

/// Sentinel `target` meaning "deliver to every connected device".
pub const BROADCAST_TARGET: &str = "broadcast";

/// Caps applied to every notification at this single enforcement point, so no
/// producer (tool call, schedule bridge, …) can persist or stream an
/// arbitrarily large payload. Truncation is by character, never by byte, so a
/// multi-byte boundary can't panic.
const MAX_TITLE_CHARS: usize = 200;
const MAX_BODY_CHARS: usize = 2000;

fn clamp(mut n: Notification) -> Notification {
    if n.title.chars().count() > MAX_TITLE_CHARS {
        n.title = n.title.chars().take(MAX_TITLE_CHARS).collect();
    }
    if n.body.chars().count() > MAX_BODY_CHARS {
        n.body = n.body.chars().take(MAX_BODY_CHARS).collect();
    }
    n
}

pub struct BroadcastNotificationSender {
    tx: broadcast::Sender<Notification>,
    queue: Arc<dyn NotificationQueueRepository>,
    relay: Option<Arc<dyn NotificationRelay>>,
}

impl BroadcastNotificationSender {
    pub fn new(
        tx: broadcast::Sender<Notification>,
        queue: Arc<dyn NotificationQueueRepository>,
        relay: Option<Arc<dyn NotificationRelay>>,
    ) -> Self {
        Self { tx, queue, relay }
    }
}

#[async_trait]
impl NotificationSender for BroadcastNotificationSender {
    async fn send(&self, notification: Notification) -> Result<()> {
        let notification = clamp(notification);
        let targeted = notification.target != BROADCAST_TARGET;
        if targeted {
            // Persist for offline delivery before fanning out live.
            self.queue.enqueue(notification.clone()).await?;
            // Background push is best-effort — never fail the send on it.
            if let Some(relay) = &self.relay {
                if let Err(e) = relay.relay(&notification).await {
                    tracing::warn!(error = %e, "notification relay failed");
                }
            }
        }
        // No subscribers is normal (no device connected) — ignore the error.
        let _ = self.tx.send(notification);
        Ok(())
    }

    async fn broadcast(&self, notification: Notification) -> Result<()> {
        // Broadcasts are ephemeral (not per-device queued).
        let mut notification = clamp(notification);
        notification.target = BROADCAST_TARGET.to_string();
        let _ = self.tx.send(notification);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct StubQueue {
        enqueued: Mutex<Vec<Notification>>,
    }
    #[async_trait]
    impl NotificationQueueRepository for StubQueue {
        async fn enqueue(&self, n: Notification) -> Result<()> {
            self.enqueued.lock().unwrap().push(n);
            Ok(())
        }
        async fn list_undelivered(&self, _device_id: &str) -> Result<Vec<Notification>> {
            Ok(self.enqueued.lock().unwrap().clone())
        }
        async fn mark_delivered(&self, _ids: &[String]) -> Result<()> {
            Ok(())
        }
    }

    fn notif(target: &str) -> Notification {
        Notification {
            id: "n1".into(),
            target: target.into(),
            category: "info".into(),
            title: "t".into(),
            body: "b".into(),
            timestamp: "2026-06-29T00:00:00Z".into(),
            data: None,
        }
    }

    #[tokio::test]
    async fn targeted_send_enqueues_and_broadcasts() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        sender.send(notif("dev-1")).await.unwrap();

        assert_eq!(queue.enqueued.lock().unwrap().len(), 1, "targeted enqueued");
        assert_eq!(rx.recv().await.unwrap().target, "dev-1", "fanned out live");
    }

    #[tokio::test]
    async fn oversized_title_and_body_are_truncated_char_safe() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        let mut n = notif("dev-1");
        // Multi-byte chars so a byte-based slice would panic at the boundary.
        n.title = "é".repeat(MAX_TITLE_CHARS + 50);
        n.body = "🦆".repeat(MAX_BODY_CHARS + 50);
        sender.send(n).await.unwrap();

        let got = rx.recv().await.unwrap();
        assert_eq!(got.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(got.body.chars().count(), MAX_BODY_CHARS);
        // The queued copy is clamped too (clamp happens before enqueue).
        let queued = &queue.enqueued.lock().unwrap()[0];
        assert_eq!(queued.body.chars().count(), MAX_BODY_CHARS);
    }

    #[tokio::test]
    async fn broadcast_does_not_enqueue() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        sender.broadcast(notif("ignored")).await.unwrap();

        assert!(
            queue.enqueued.lock().unwrap().is_empty(),
            "broadcast not queued"
        );
        assert_eq!(rx.recv().await.unwrap().target, BROADCAST_TARGET);
    }
}
