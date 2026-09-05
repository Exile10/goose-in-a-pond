use serde::{Deserialize, Serialize};

/// Configuration for one external service (e.g. Spotify) GIAP authenticates
/// against with OAuth 2.1 Authorization Code + PKCE. `bundled_client_id` ships
/// with the binary; a user override is read from the secret store under
/// `{PROVIDER_ID}_CLIENT_ID` (uppercase).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthProviderConfig {
    /// Provider identifier (e.g. "spotify").
    pub id: String,
    /// Human-readable display name (e.g. "Spotify").
    pub display_name: String,
    /// Authorization endpoint URL.
    pub authorize_url: String,
    /// Token exchange endpoint URL.
    pub token_url: String,
    /// Required OAuth scopes.
    pub scopes: Vec<String>,
    /// GIAP's bundled Client ID for this provider.
    pub bundled_client_id: String,
    /// Secret key name under which the access token is stored.
    pub token_key: String,
    /// Secret key name under which the refresh token is stored.
    pub refresh_key: String,
}
