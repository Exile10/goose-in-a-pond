//! Push-notification token registered by a paired GOTG mobile device (#95).
//!
//! Stored one-per-device so the push path (#99) can reach a backgrounded phone
//! via FCM/APNs using the device's current token.
//!
//! # A token has no owner of its own
//!
//! PAI-1's device-to-profile rung deliberately did **not** add a `profile_id`
//! here. A token belongs to a device and a device belongs (or does not belong)
//! to a household member, so the association lives once, on `devices.profile_id`
//! (migration 0043), and is read through
//! [`DeviceAttribution`](crate::user_data::ports::device_attribution::DeviceAttribution).
//!
//! Copying it onto the token row as well would give the delivery path two
//! writable answers to "whose phone is this?", and `PushTokenRepository::upsert`
//! -- called on every token refresh, from the device itself -- would be one of
//! the writers. A phone that re-registered its token could then change who it
//! belonged to. The disagreement would be invisible until a proposal addressed
//! to one member arrived on another member's screen, which is the one outcome
//! this workstream exists to prevent.

use serde::{Deserialize, Serialize};

/// Which push service a token targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushPlatform {
    /// Firebase Cloud Messaging (Android).
    Fcm,
    /// Apple Push Notification service (iOS).
    Apns,
    /// Expo push service (Expo-managed mobile clients).
    Expo,
}

impl PushPlatform {
    /// Parse from the snake_case wire form (`"fcm"` | `"apns"` | `"expo"`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fcm" => Some(Self::Fcm),
            "apns" => Some(Self::Apns),
            "expo" => Some(Self::Expo),
            _ => None,
        }
    }

    /// The snake_case wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fcm => "fcm",
            Self::Apns => "apns",
            Self::Expo => "expo",
        }
    }
}

/// A device's current push-notification token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushToken {
    /// The owning device's id (FK to `devices.id`).
    pub device_id: String,
    /// The opaque platform-issued token.
    pub token: String,
    pub platform: PushPlatform,
    /// RFC3339 timestamp of the last update.
    pub updated_at: String,
}
