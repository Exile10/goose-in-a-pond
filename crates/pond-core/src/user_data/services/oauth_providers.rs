//! Built-in OAuth PKCE providers. Each household's client ID is the `{PROVIDER_ID}_CLIENT_ID` secret.

use crate::security::domain::oauth_provider::OAuthProviderConfig;
use crate::security::ports::secret::SecretRepository;

pub fn builtin_oauth_providers() -> Vec<OAuthProviderConfig> {
    vec![OAuthProviderConfig {
        id: "spotify".to_string(),
        display_name: "Spotify".to_string(),
        authorize_url: "https://accounts.spotify.com/authorize".to_string(),
        token_url: "https://accounts.spotify.com/api/token".to_string(),
        // Only what the app's own Spotify features use (Developer Terms V.3: ask for no more): the
        // Web Playback SDK needs the last three, the music controls read and change playback. The
        // assistant never controls Spotify, so nothing here reads playlists, the library or history.
        // New scopes only reach tokens issued after the user signs in again.
        scopes: vec![
            "user-read-playback-state".to_string(),
            "user-modify-playback-state".to_string(),
            "user-read-currently-playing".to_string(),
            "streaming".to_string(),
            "user-read-email".to_string(),
            "user-read-private".to_string(),
        ],
        // Spotify's Developer Terms VI.1: a client ID is a Security Code, not to be disclosed, and
        // development mode admits five allowlisted users. So none ships here: each household
        // registers its own app and stores its ID as SPOTIFY_CLIENT_ID.
        bundled_client_id: None,
        token_key: "SPOTIFY_ACCESS_TOKEN".to_string(),
        refresh_key: "SPOTIFY_REFRESH_TOKEN".to_string(),
    }]
}

/// The OAuth `error` code in a token endpoint's refusal, if its body carries one.
pub fn token_error_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value.get("error")?.as_str().map(str::to_string)
}

/// Whether a token endpoint's refusal of a refresh means the refresh token will never work again:
/// a 400 whose `error` is `invalid_grant` (RFC 6749 section 5.2), which is what a revoked or
/// expired refresh token gets. Any other refusal, such as a wrong client ID or an outage, may
/// clear up on its own, so it is no reason to forget the sign-in.
pub fn refresh_token_rejected(status: u16, body: &str) -> bool {
    status == 400 && token_error_code(body).as_deref() == Some("invalid_grant")
}

/// Forgets a sign-in whose refresh token the provider has rejected for good.
///
/// Both tokens go. Without the refresh token nothing retries it, where before every refresh tick
/// and every 401 from the provider's API sent it again; without the access token the service
/// reads as not connected, which is how the dashboard and the phone already ask for a new sign-in.
/// Logged here, once, because nothing refreshes the sign-in afterwards.
pub async fn forget_rejected_sign_in(repo: &dyn SecretRepository, provider: &OAuthProviderConfig) {
    for key in [&provider.token_key, &provider.refresh_key] {
        if let Err(e) = repo.delete(key).await {
            tracing::warn!(
                kind = "oauth.sign_in_forget_failed",
                provider = %provider.id,
                key = %key,
                error = %e,
                "could not forget a rejected sign-in; the refresh will be retried"
            );
        }
    }
    tracing::warn!(
        kind = "oauth.sign_in_rejected",
        provider = %provider.id,
        "{} rejected the saved sign-in; it has been forgotten, so sign in to {} again",
        provider.display_name,
        provider.display_name
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_providers_contains_spotify() {
        let providers = builtin_oauth_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "spotify");
        assert_eq!(providers[0].display_name, "Spotify");
        assert!(!providers[0].scopes.is_empty());
        for needed in ["streaming", "user-read-email", "user-read-private"] {
            assert!(
                providers[0].scopes.iter().any(|s| s == needed),
                "the Spotify player page's SDK needs the {needed} scope"
            );
        }
        // Developer Terms V.3: no more than the app's own features use.
        let mut asked: Vec<&str> = providers[0].scopes.iter().map(String::as_str).collect();
        asked.sort_unstable();
        assert_eq!(
            asked,
            [
                "streaming",
                "user-modify-playback-state",
                "user-read-currently-playing",
                "user-read-email",
                "user-read-playback-state",
                "user-read-private",
            ],
            "the assistant never controls Spotify, so no playlist, library or history scope"
        );
        assert_eq!(
            providers[0].bundled_client_id, None,
            "no Spotify client ID ships with GIAP: each household uses its own"
        );
        assert_eq!(providers[0].token_key, "SPOTIFY_ACCESS_TOKEN");
        assert_eq!(providers[0].refresh_key, "SPOTIFY_REFRESH_TOKEN");
    }

    #[test]
    fn provider_config_is_serializable() {
        let providers = builtin_oauth_providers();
        let json = serde_json::to_string(&providers[0]).unwrap();
        assert!(json.contains("spotify"));
        let roundtrip: OAuthProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.id, "spotify");
    }

    #[test]
    fn only_an_invalid_grant_400_rejects_the_refresh_token() {
        let revoked = r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#;
        assert!(refresh_token_rejected(400, revoked));
        // A wrong client ID is the household's to fix; the sign-in itself may still be good.
        assert!(!refresh_token_rejected(
            400,
            r#"{"error":"invalid_client","error_description":"Invalid client"}"#
        ));
        // An outage, a rate limit or a body that is not OAuth's says nothing about the token.
        assert!(!refresh_token_rejected(503, revoked));
        assert!(!refresh_token_rejected(429, revoked));
        assert!(!refresh_token_rejected(400, "<html>Bad Request</html>"));
        assert!(!refresh_token_rejected(400, ""));
    }

    #[tokio::test]
    async fn a_rejected_sign_in_is_forgotten_so_nothing_retries_it() {
        use std::collections::HashMap;
        use std::sync::Mutex;

        struct Store(Mutex<HashMap<String, String>>);
        #[async_trait::async_trait]
        impl SecretRepository for Store {
            async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
                Ok(self.0.lock().unwrap().get(key).cloned())
            }
            async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .insert(key.to_string(), value.to_string());
                Ok(())
            }
            async fn delete(&self, key: &str) -> anyhow::Result<()> {
                self.0.lock().unwrap().remove(key);
                Ok(())
            }
            async fn list_keys(&self) -> anyhow::Result<Vec<String>> {
                Ok(self.0.lock().unwrap().keys().cloned().collect())
            }
            async fn has(&self, key: &str) -> anyhow::Result<bool> {
                Ok(self.0.lock().unwrap().contains_key(key))
            }
        }

        let store = Store(Mutex::new(HashMap::from([
            ("SPOTIFY_ACCESS_TOKEN".to_string(), "access".to_string()),
            ("SPOTIFY_REFRESH_TOKEN".to_string(), "refresh".to_string()),
            ("SPOTIFY_CLIENT_ID".to_string(), "client".to_string()),
        ])));
        let spotify = builtin_oauth_providers().remove(0);

        forget_rejected_sign_in(&store, &spotify).await;

        // The refresh worker and the API call both stop at a missing token.
        assert!(!store.has("SPOTIFY_REFRESH_TOKEN").await.unwrap());
        assert!(!store.has("SPOTIFY_ACCESS_TOKEN").await.unwrap());
        // The household's own app registration is not the sign-in: signing in again needs it.
        assert!(store.has("SPOTIFY_CLIENT_ID").await.unwrap());
    }
}
