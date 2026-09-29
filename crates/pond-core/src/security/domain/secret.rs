use serde::{Deserialize, Serialize};

/// Describes a secret that an extension needs to function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRequirement {
    /// Environment variable name (e.g. "SPOTIFY_ACCESS_TOKEN")
    pub key: String,
    /// Human-readable label (e.g. "Spotify")
    pub display_name: String,
    /// Help text (e.g. "Sign in to control playback")
    pub description: String,
    /// Whether the extension won't work without this secret
    pub required: bool,
    pub kind: SecretKind,
    /// Used by the host only (e.g. a signing key), so never put in the extension's environment.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub host_only: bool,
    /// For someone bringing their own credentials or overriding a default. The UI keeps these
    /// under "Developer settings" so the ordinary path is a sign-in button, not a form.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub advanced: bool,
}

/// How a secret is obtained by the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    /// Manual paste (GitHub token, Brave API key)
    ApiKey,
    /// One-click "Sign in with X" — GIAP handles the entire OAuth PKCE flow
    #[serde(rename = "oauth_flow")]
    OAuthFlow,
    /// Free-form text input
    Generic,
}
