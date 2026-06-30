//! `NotificationSender` over an in-process broadcast channel + offline queue (#99).
//!
//! - foreground: publish on a `tokio::sync::broadcast` channel that connected
//!   `/notifications/stream` clients subscribe to;
//! - offline: persist targeted notifications to the queue so a disconnected
//!   device gets them on reconnect;
//! - background (scaffold): hand targeted notifications to the optional
//!   [`NotificationRelay`] (FCM/APNs), best-effort.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification::{Notification, NotificationSender};
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use pond_core::mcp::ports::notification_relay::NotificationRelay;
use tokio::sync::broadcast;

/// Sentinel `target` meaning "deliver to every connected device".
pub const BROADCAST_TARGET: &str = "broadcast";

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

    async fn broadcast(&self, mut notification: Notification) -> Result<()> {
        // Broadcasts are ephemeral (not per-device queued).
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
