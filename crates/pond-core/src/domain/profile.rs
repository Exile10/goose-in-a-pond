//! Profile domain types — represents a household member.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A household member profile.
///
/// `preferences` is a flat string-to-string map (avoids `serde_json::Value`
/// dependency in pond-core). Callers in pond-api convert to/from JSON objects
/// when crossing the HTTP boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub display_name: String,
    /// Single emoji representing this profile (default: duck emoji)
    pub avatar_emoji: String,
    /// Arbitrary string key-value preferences
    pub preferences: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for creating a new profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProfileRequest {
    pub display_name: String,
    #[serde(default = "default_avatar")]
    pub avatar_emoji: String,
}

fn default_avatar() -> String {
    "\u{1F986}".to_string() // 🦆
}
