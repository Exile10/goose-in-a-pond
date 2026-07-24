//! Stub FCM/APNs relay (#99, Phase 2 scaffold).
//!
//! Resolves a device's stored push token (#95) and *logs* the intended
//! background push instead of really sending it. Replacing this with real
//! FCM-v1 / APNs HTTP is a follow-up (needs service credentials + a device).
//! The token is never logged in full.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification::Notification;
use pond_core::mcp::ports::notification_relay::NotificationRelay;
use pond_core::user_data::ports::push_token::PushTokenRepository;

use crate::push_token_log::token_log_prefix;

pub struct StubPushRelay {
    push_tokens: Arc<dyn PushTokenRepository>,
}

impl StubPushRelay {
    pub fn new(push_tokens: Arc<dyn PushTokenRepository>) -> Self {
        Self { push_tokens }
    }
}

#[async_trait]
impl NotificationRelay for StubPushRelay {
    async fn relay(&self, notification: &Notification) -> Result<()> {
        let Some(token) = self.push_tokens.get(&notification.target).await? else {
            tracing::debug!(
                device = %notification.target,
                "push relay: no token registered; skipping background push"
            );
            return Ok(());
        };
        let prefix = token_log_prefix(&token.token);
        // Scaffold: real FCM/APNs delivery is a follow-up.
        tracing::info!(
            device = %notification.target,
            platform = token.platform.as_str(),
            token_prefix = %prefix,
            category = %notification.category,
            "push relay (stub): would deliver background push"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::push_token::{PushPlatform, PushToken};
    use std::sync::Mutex;

    #[derive(Default)]
    struct StubTokens {
        token: Mutex<Option<PushToken>>,
    }
    #[async_trait]
    impl PushTokenRepository for StubTokens {
        async fn upsert(&self, t: PushToken) -> Result<()> {
            *self.token.lock().unwrap() = Some(t);
            Ok(())
        }
        async fn get(&self, _device_id: &str) -> Result<Option<PushToken>> {
            Ok(self.token.lock().unwrap().clone())
        }
        async fn list(&self) -> Result<Vec<PushToken>> {
            Ok(self.token.lock().unwrap().clone().into_iter().collect())
        }
        async fn delete(&self, _device_id: &str) -> Result<()> {
            *self.token.lock().unwrap() = None;
            Ok(())
        }
    }

    fn notif() -> Notification {
        Notification {
            id: "n1".into(),
            target: "dev-1".into(),
            category: "info".into(),
            title: "t".into(),
            body: "b".into(),
            timestamp: "2026-06-29T00:00:00Z".into(),
            data: None,
        }
    }

    #[tokio::test]
    async fn relay_is_ok_with_and_without_token() {
        let tokens = Arc::new(StubTokens::default());
        let relay = StubPushRelay::new(tokens.clone());

        // No token registered — still Ok (best-effort).
        relay.relay(&notif()).await.unwrap();

        tokens
            .upsert(PushToken {
                device_id: "dev-1".into(),
                token: "ExponentPushToken[secret]".into(),
                platform: PushPlatform::Expo,
                updated_at: String::new(),
            })
            .await
            .unwrap();
        relay.relay(&notif()).await.unwrap();
    }

    /// Regression: a token whose 8th byte falls inside a multi-byte character
    /// used to panic while building the log prefix. `relay` is awaited inline
    /// in `BroadcastNotificationSender::send`, which catches errors but not
    /// panics, so this would have taken down the whole notification send.
    #[tokio::test]
    async fn relay_does_not_panic_on_a_multi_byte_token() {
        let tokens = Arc::new(StubTokens::default());
        let relay = StubPushRelay::new(tokens.clone());
        tokens
            .upsert(PushToken {
                device_id: "dev-1".into(),
                token: "日本語のトークンです".into(),
                platform: PushPlatform::Fcm,
                updated_at: String::new(),
            })
            .await
            .unwrap();
        relay.relay(&notif()).await.unwrap();
    }
}
