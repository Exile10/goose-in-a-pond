//! GOTG Notification Adapter
//!
//! Implements the NotificationSender port for pushing events to
//! connected GOTG mobile apps.
//!
//! # TODO
//! - [ ] Implement WebSocket or SSE transport for real-time push
//! - [ ] Add notification queue for offline devices
//! - [ ] Add push notification (FCM/APNs) for background delivery
//! - [ ] Add notification deduplication

use anyhow::Result;
use async_trait::async_trait;
use pond_core::ports::notification::{Notification, NotificationSender};

/// GOTG-specific notification adapter.
///
/// Currently logs notifications. Will send via WebSocket/SSE when implemented.
pub struct GotgNotificationAdapter {
    // TODO: Add WebSocket connection pool, notification queue
}

impl GotgNotificationAdapter {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl NotificationSender for GotgNotificationAdapter {
    async fn send(&self, notification: Notification) -> Result<()> {
        // TODO: Push via WebSocket/SSE to the target device
        tracing::info!(
            "Notification → {}: {} — {}",
            notification.target,
            notification.title,
            notification.body
        );
        Ok(())
    }

    async fn broadcast(&self, notification: Notification) -> Result<()> {
        // TODO: Push to all connected GOTG devices
        tracing::info!(
            "Broadcast: {} — {}",
            notification.title,
            notification.body
        );
        Ok(())
    }
}
