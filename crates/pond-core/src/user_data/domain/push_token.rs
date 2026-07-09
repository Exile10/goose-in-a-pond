//! Push-notification token registered by a paired GOTG mobile device (#95).
//!
//! Stored one-per-device so the push path (#99) can reach a backgrounded phone
//! via FCM/APNs using the device's current token.

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
