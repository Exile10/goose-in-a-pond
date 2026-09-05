//! Driven port: background push relay (#99, Phase 2 scaffold). Delivers a notification to a
//! backgrounded device via FCM or APNs, resolving the device's stored push token (#95).
//! Best-effort: a missing token or a transient failure must not break foreground delivery.
//! The real FCM/APNs HTTP client lands in a follow-up; today a stub logs the intent.

use anyhow::Result;
use async_trait::async_trait;

use crate::mcp::ports::notification::Notification;

#[async_trait]
pub trait NotificationRelay: Send + Sync {
    /// Relay a targeted notification to its device's background push channel.
    async fn relay(&self, notification: &Notification) -> Result<()>;
}
