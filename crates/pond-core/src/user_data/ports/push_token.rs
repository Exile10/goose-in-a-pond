//! Driven port: persistence for device push-notification tokens (#95).
//!
//! One current token per device. Powers the push-notification path (#99), which
//! reads stored tokens to reach backgrounded mobile clients.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::push_token::PushToken;

#[async_trait]
pub trait PushTokenRepository: Send + Sync {
    /// Insert or replace the token for a device (one current token per device).
    async fn upsert(&self, token: PushToken) -> Result<()>;

    /// The current token for a device, if one is registered.
    async fn get(&self, device_id: &str) -> Result<Option<PushToken>>;

    /// Every registered token, attributed or not.
    ///
    /// This is the **broadcast** set. It is not the set of devices belonging to
    /// any particular household member, and PAI-7's invariant 4 (proposals are
    /// addressed to a profile, never broadcast) is broken by using it as one.
    /// For a targeted send use
    /// [`DeviceAttribution::push_tokens_for_profile`](crate::user_data::ports::device_attribution::DeviceAttribution::push_tokens_for_profile).
    async fn list(&self) -> Result<Vec<PushToken>>;

    /// Remove a device's token (logout / unpair).
    async fn delete(&self, device_id: &str) -> Result<()>;
}
