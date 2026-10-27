//! Test double for [`NotificationSender`]: records every notification and how it was addressed.

use std::sync::Mutex;

use anyhow::{bail, Result};
use async_trait::async_trait;

use crate::mcp::ports::notification::{Notification, NotificationSender};

/// How a recorded notification was addressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// `send()`: to `Notification::target`.
    Send,
    /// `publish_live()`: connected screens only.
    Live,
    /// `broadcast()`: the household.
    Broadcast,
    /// `broadcast_except()`: the household but this device.
    BroadcastExcept(String),
}

#[derive(Default)]
pub struct MockNotificationSender {
    sent: Mutex<Vec<(Delivery, Notification)>>,
    failing: bool,
}

impl MockNotificationSender {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call fails, as a sender whose queue cannot be written would; nothing is recorded.
    pub fn failing(mut self) -> Self {
        self.failing = true;
        self
    }

    /// Every notification handed over, in order.
    pub fn sent(&self) -> Vec<(Delivery, Notification)> {
        self.sent.lock().unwrap().clone()
    }

    fn record(&self, delivery: Delivery, notification: Notification) -> Result<()> {
        if self.failing {
            bail!("the notification queue could not be written");
        }
        self.sent.lock().unwrap().push((delivery, notification));
        Ok(())
    }
}

#[async_trait]
impl NotificationSender for MockNotificationSender {
    async fn send(&self, notification: Notification) -> Result<()> {
        self.record(Delivery::Send, notification)
    }

    async fn publish_live(&self, notification: Notification) -> Result<()> {
        self.record(Delivery::Live, notification)
    }

    async fn broadcast(&self, notification: Notification) -> Result<()> {
        self.record(Delivery::Broadcast, notification)
    }

    async fn broadcast_except(
        &self,
        notification: Notification,
        except_device: &str,
    ) -> Result<()> {
        self.record(
            Delivery::BroadcastExcept(except_device.to_string()),
            notification,
        )
    }
}
