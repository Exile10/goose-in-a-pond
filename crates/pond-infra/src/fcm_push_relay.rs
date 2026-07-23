//! Real FCM-v1 push relay (#99 Phase 2) — replaces [`crate::stub_push_relay`]
//! when a Firebase service-account key is present on disk.
//!
//! Privacy design: GIAP sends **data-only wake pings** — an opaque
//! `{notification_id, category, wake}` payload with NO title or body — so
//! notification content never transits Google's servers. The phone wakes and
//! pulls the actual content from GIAP over its authenticated channel.
//!
//! Credentials: the service-account JSON is read from
//! `<data_dir>/secrets/fcm-service-account.json` (override with
//! `POND_FCM_KEY_PATH`). It is a SECRET — it lives outside the repo, is never
//! logged, and only its `client_email`/`project_id` identifiers appear in
//! logs. Auth is the standard OAuth2 JWT-bearer flow: sign a short-lived
//! RS256 assertion with the key, exchange it for an access token (cached
//! until shortly before expiry), and call
//! `https://fcm.googleapis.com/v1/projects/<id>/messages:send`.
//!
//! Every outbound call (token exchange + send) is recorded through the #113
//! egress tracker, so pushes appear in the activity feed like any other
//! network traffic — a local-first assistant that tells you when it talked
//! to Google.
//!
//! APNs (iOS) is not implemented yet: it needs an Apple Developer membership
//! for the signing key. Tokens registered with platform `apns`/`expo` are
//! skipped with a log line.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pond_core::mcp::ports::notification::Notification;
use pond_core::mcp::ports::notification_relay::NotificationRelay;
use pond_core::shared::services::egress::record_egress;
use pond_core::user_data::domain::push_token::PushPlatform;
use pond_core::user_data::ports::push_token::PushTokenRepository;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::push_token_log::token_log_prefix;

/// OAuth scope for FCM v1 sends.
const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
/// Refresh the cached access token this long before its stated expiry.
const TOKEN_EXPIRY_MARGIN: Duration = Duration::from_secs(60);
/// Ceiling on each outbound Google call. The relay is awaited inline in
/// `BroadcastNotificationSender::send`, so an unbounded request would stall
/// notification delivery behind a hung socket.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// The fields GIAP needs from a Firebase service-account key file.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceAccount {
    pub project_id: String,
    pub private_key: String,
    pub client_email: String,
    pub token_uri: String,
}

/// Parse a service-account JSON (no key validation here — that happens when
/// the signing key is constructed at startup).
pub fn parse_service_account(raw: &str) -> Result<ServiceAccount> {
    serde_json::from_str(raw).context("service-account JSON missing required fields")
}

/// Google's FCM v1 host. Overridable only so tests can point the send at a
/// local mock; production always uses this.
const FCM_BASE_URL: &str = "https://fcm.googleapis.com";

/// The FCM v1 send endpoint for a project, under `base`.
pub fn fcm_send_url(base: &str, project_id: &str) -> String {
    format!("{base}/v1/projects/{project_id}/messages:send")
}

/// Build the data-only wake message for a device token. Deliberately carries
/// NO `notification` block and NO title/body: content stays on the Pond.
pub fn wake_message(device_token: &str, notification: &Notification) -> Value {
    json!({
        "message": {
            "token": device_token,
            "data": {
                "notification_id": notification.id,
                "category": notification.category,
                "wake": "1",
            },
            "android": { "priority": "HIGH" },
        }
    })
}

#[derive(serde::Serialize)]
struct Claims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

pub struct FcmPushRelay {
    account: ServiceAccount,
    signing_key: jsonwebtoken::EncodingKey,
    push_tokens: Arc<dyn PushTokenRepository>,
    http: reqwest::Client,
    /// `(access_token, refresh_after)` — refreshed lazily on demand.
    cached_token: Mutex<Option<(String, Instant)>>,
    /// FCM host. Always [`FCM_BASE_URL`] outside tests.
    fcm_base: String,
}

impl FcmPushRelay {
    /// Load and validate the service-account key file. Fails fast (bad path,
    /// malformed JSON, non-RSA key) so the caller can fall back to the stub.
    pub fn from_key_file(path: &Path, push_tokens: Arc<dyn PushTokenRepository>) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading FCM service-account key at {}", path.display()))?;
        let account = parse_service_account(&raw)?;
        let signing_key = jsonwebtoken::EncodingKey::from_rsa_pem(account.private_key.as_bytes())
            .context("service-account private_key is not a valid RSA PEM")?;
        tracing::info!(
            project = %account.project_id,
            client = %account.client_email,
            "FCM push relay active (data-only wake pings)"
        );
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .context("building the FCM HTTP client")?;
        Ok(Self {
            account,
            signing_key,
            push_tokens,
            http,
            cached_token: Mutex::new(None),
            fcm_base: FCM_BASE_URL.to_string(),
        })
    }

    /// A valid OAuth access token, from cache or via a fresh JWT-bearer
    /// exchange.
    async fn access_token(&self) -> Result<String> {
        if let Some((token, refresh_after)) = self
            .cached_token
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            if Instant::now() < refresh_after {
                return Ok(token);
            }
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock before UNIX epoch")?
            .as_secs();
        let claims = Claims {
            iss: &self.account.client_email,
            scope: FCM_SCOPE,
            aud: &self.account.token_uri,
            iat: now,
            exp: now + 3600,
        };
        let assertion = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &self.signing_key,
        )
        .context("signing FCM auth assertion")?;

        let started = Instant::now();
        let response = self
            .http
            .post(&self.account.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await;
        let latency = started.elapsed().as_millis() as u64;
        let response = match response {
            Ok(r) => {
                record_egress(
                    &self.account.token_uri,
                    "POST",
                    Some(r.status().as_u16()),
                    latency,
                );
                r
            }
            Err(e) => {
                record_egress(&self.account.token_uri, "POST", None, latency);
                return Err(anyhow!("FCM token exchange failed: {e}"));
            }
        };
        if !response.status().is_success() {
            return Err(anyhow!(
                "FCM token exchange rejected (HTTP {})",
                response.status()
            ));
        }
        let token: TokenResponse = response
            .json()
            .await
            .context("parsing FCM token response")?;

        let refresh_after = Instant::now()
            + Duration::from_secs(token.expires_in).saturating_sub(TOKEN_EXPIRY_MARGIN);
        *self.cached_token.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((token.access_token.clone(), refresh_after));
        Ok(token.access_token)
    }
}

#[async_trait]
impl NotificationRelay for FcmPushRelay {
    async fn relay(&self, notification: &Notification) -> Result<()> {
        let Some(stored) = self.push_tokens.get(&notification.target).await? else {
            tracing::debug!(
                device = %notification.target,
                "FCM relay: no push token registered; skipping background push"
            );
            return Ok(());
        };
        match stored.platform {
            PushPlatform::Fcm => {}
            PushPlatform::Apns => {
                tracing::info!(
                    device = %notification.target,
                    "APNs push not yet supported (needs an Apple Developer key); skipping"
                );
                return Ok(());
            }
            PushPlatform::Expo => {
                tracing::debug!(
                    device = %notification.target,
                    "expo-platform token registered but GIAP relays directly; skipping"
                );
                return Ok(());
            }
        }

        let access_token = self.access_token().await?;
        let url = fcm_send_url(&self.fcm_base, &self.account.project_id);
        let body = wake_message(&stored.token, notification);

        let started = Instant::now();
        let response = self
            .http
            .post(&url)
            .bearer_auth(access_token)
            .json(&body)
            .send()
            .await;
        let latency = started.elapsed().as_millis() as u64;
        let response = match response {
            Ok(r) => {
                record_egress(&url, "POST", Some(r.status().as_u16()), latency);
                r
            }
            Err(e) => {
                record_egress(&url, "POST", None, latency);
                return Err(anyhow!("FCM send failed: {e}"));
            }
        };

        let prefix = token_log_prefix(&stored.token);
        if response.status().is_success() {
            tracing::info!(
                device = %notification.target,
                token_prefix = %prefix,
                category = %notification.category,
                "FCM wake ping delivered"
            );
            Ok(())
        } else {
            // 404/410 = stale token (app reinstalled); others = config/quota.
            Err(anyhow!(
                "FCM rejected wake ping for device {} (HTTP {})",
                notification.target,
                response.status()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::push_token::PushToken;

    /// A relay whose signing key is deliberately NOT an RSA key. Every test
    /// using it exercises a branch that returns before anything is signed, so
    /// reaching `encode` would fail the test loudly rather than silently pass.
    /// This keeps the suite free of a committed PEM — gitleaks scans the full
    /// history, and a private key put there could not be taken back.
    fn relay_with(tokens: Arc<dyn PushTokenRepository>) -> FcmPushRelay {
        FcmPushRelay {
            account: ServiceAccount {
                project_id: "goose-test".into(),
                private_key: "unused".into(),
                client_email: "svc@goose-test.iam.gserviceaccount.com".into(),
                // Unroutable by design: if a test ever reached the network,
                // it would hang or fail rather than quietly talk to Google.
                token_uri: "http://127.0.0.1:1/token".into(),
            },
            signing_key: jsonwebtoken::EncodingKey::from_secret(b"not-an-rsa-key"),
            push_tokens: tokens,
            http: reqwest::Client::new(),
            cached_token: Mutex::new(None),
            // Unroutable for the same reason as token_uri above.
            fcm_base: "http://127.0.0.1:1".into(),
        }
    }

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
            id: "n-1".into(),
            target: "dev-1".into(),
            category: "alert".into(),
            title: "t".into(),
            body: "b".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            data: None,
        }
    }

    async fn seeded(platform: PushPlatform, token: &str) -> Arc<StubTokens> {
        let tokens = Arc::new(StubTokens::default());
        tokens
            .upsert(PushToken {
                device_id: "dev-1".into(),
                token: token.into(),
                platform,
                updated_at: String::new(),
            })
            .await
            .unwrap();
        tokens
    }

    /// A device with no registered token is a normal, silent no-op — the phone
    /// simply has no background channel yet.
    #[tokio::test]
    async fn relay_without_a_registered_token_is_a_no_op() {
        let relay = relay_with(Arc::new(StubTokens::default()));
        relay.relay(&notif()).await.unwrap();
    }

    /// APNs and Expo tokens are skipped by design (no Apple signing key yet;
    /// Expo is not on this delivery path). Both must return before signing.
    #[tokio::test]
    async fn non_fcm_platforms_are_skipped_without_touching_the_network() {
        for platform in [PushPlatform::Apns, PushPlatform::Expo] {
            let relay = relay_with(seeded(platform, "token-abcdefghij").await);
            relay
                .relay(&notif())
                .await
                .expect("non-FCM platforms are skipped, not errors");
        }
    }

    // ── Signing + HTTP path ─────────────────────────────────────────────────
    //
    // These drive the real OAuth2 JWT-bearer exchange and FCM send against a
    // local mock. The RSA key is generated at run time and never touches disk:
    // gitleaks scans the full history, so a committed PEM could not be
    // withdrawn even after deletion.

    /// A 2048-bit RSA key in PKCS#8 PEM, generated once per test binary.
    /// Key generation costs a second or so, so it is shared across tests.
    fn test_signing_key() -> &'static jsonwebtoken::EncodingKey {
        use rsa::pkcs8::EncodePrivateKey;
        static KEY: std::sync::OnceLock<jsonwebtoken::EncodingKey> = std::sync::OnceLock::new();
        KEY.get_or_init(|| {
            let mut rng = rand::thread_rng();
            // jsonwebtoken/ring requires >= 2047 bits for RS256.
            let private = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("generate RSA key");
            let pem = private
                .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
                .expect("encode PKCS#8 PEM");
            jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).expect("load generated key")
        })
    }

    /// A relay that can genuinely sign, pointed at `base` for sends and at
    /// `token_uri` for the OAuth exchange.
    fn signing_relay_with_token_uri(
        base: &str,
        token_uri: &str,
        tokens: Arc<dyn PushTokenRepository>,
    ) -> FcmPushRelay {
        FcmPushRelay {
            account: ServiceAccount {
                project_id: "goose-test".into(),
                private_key: "unused-once-the-key-is-built".into(),
                client_email: "svc@goose-test.iam.gserviceaccount.com".into(),
                token_uri: token_uri.into(),
            },
            signing_key: test_signing_key().clone(),
            push_tokens: tokens,
            http: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .unwrap(),
            cached_token: Mutex::new(None),
            fcm_base: base.into(),
        }
    }

    fn signing_relay(base: &str, tokens: Arc<dyn PushTokenRepository>) -> FcmPushRelay {
        signing_relay_with_token_uri(base, &format!("{base}/token"), tokens)
    }

    fn ok_token_response() -> wiremock::ResponseTemplate {
        wiremock::ResponseTemplate::new(200)
            .set_body_json(json!({ "access_token": "test-access-token", "expires_in": 3600 }))
    }

    /// The happy path end to end: sign an assertion, exchange it for an access
    /// token, then send the wake ping bearing that token.
    #[tokio::test]
    async fn signs_exchanges_and_sends() {
        use wiremock::matchers::{body_string_contains, header, method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            // The RFC 7523 grant, carrying our signed assertion.
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer",
            ))
            .and(body_string_contains("assertion="))
            .respond_with(ok_token_response())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/goose-test/messages:send"))
            .and(header("authorization", "Bearer test-access-token"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(json!({ "name": "ok" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "fcm-device-token").await,
        );
        relay.relay(&notif()).await.expect("wake ping delivered");
        // `expect(...)` on both mocks is verified when the server drops.
    }

    /// The access token is cached: a second push in the same window must not
    /// re-run the exchange. Google rate-limits token issuance, and this is
    /// also what keeps a burst of notifications from doubling our egress.
    #[tokio::test]
    async fn the_access_token_is_reused_across_sends() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ok_token_response())
            .expect(1) // exactly once, despite two sends
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/goose-test/messages:send"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(2)
            .mount(&server)
            .await;

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "fcm-device-token").await,
        );
        relay.relay(&notif()).await.unwrap();
        relay.relay(&notif()).await.unwrap();
    }

    /// A rejected exchange must surface as an error rather than a send with no
    /// credentials attached.
    #[tokio::test]
    async fn a_rejected_token_exchange_fails_without_sending() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(wiremock::ResponseTemplate::new(401).set_body_json(json!({
                "error": "invalid_grant"
            })))
            .mount(&server)
            .await;
        // No send mock: reaching the send endpoint at all fails the test.

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "fcm-device-token").await,
        );
        let err = relay.relay(&notif()).await.expect_err("must not proceed");
        assert!(
            err.to_string().contains("token exchange rejected"),
            "unexpected error: {err}"
        );
    }

    /// 404/410 from FCM means a stale token (app reinstalled). It must be
    /// reported, not swallowed — the sender logs it, and a swallowed failure
    /// would look like successful delivery.
    #[tokio::test]
    async fn a_rejected_send_is_reported() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ok_token_response())
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/goose-test/messages:send"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "fcm-device-token").await,
        );
        let err = relay.relay(&notif()).await.expect_err("404 must surface");
        assert!(err.to_string().contains("404"), "unexpected error: {err}");
    }

    /// A token whose 8th byte falls inside a multi-byte character reaches the
    /// redaction path only after a successful send — the case the earlier
    /// tests could not get to. This is the panic guard on this relay's own
    /// logging.
    #[tokio::test]
    async fn a_multi_byte_token_survives_the_send_path() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ok_token_response())
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/goose-test/messages:send"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let relay = signing_relay(
            &server.uri(),
            seeded(PushPlatform::Fcm, "日本語のトークンです").await,
        );
        relay
            .relay(&notif())
            .await
            .expect("must not panic redacting");
    }

    #[test]
    fn parses_a_service_account_and_rejects_incomplete_ones() {
        // Structure-only fixture; the fake key is a plain string, not a PEM.
        let ok = r#"{
            "type": "service_account",
            "project_id": "goose-test",
            "private_key": "not-a-real-key",
            "client_email": "svc@goose-test.iam.gserviceaccount.com",
            "token_uri": "https://oauth2.googleapis.com/token"
        }"#;
        let account = parse_service_account(ok).unwrap();
        assert_eq!(account.project_id, "goose-test");
        assert!(parse_service_account(r#"{"project_id": "x"}"#).is_err());
    }

    #[test]
    fn send_url_targets_the_project() {
        assert_eq!(
            fcm_send_url(FCM_BASE_URL, "goose-test"),
            "https://fcm.googleapis.com/v1/projects/goose-test/messages:send"
        );
    }

    /// Production must never be pointed anywhere but Google. The override
    /// exists for the mock server below and nothing else.
    #[test]
    fn the_default_base_is_googles() {
        let relay = signing_relay("http://unused", Arc::new(StubTokens::default()));
        assert_eq!(FCM_BASE_URL, "https://fcm.googleapis.com");
        assert!(relay.fcm_base.starts_with("http://")); // test override in effect
    }

    /// The privacy contract: wake pings are data-only — no `notification`
    /// block, no title, no body anywhere in the payload.
    #[test]
    fn wake_messages_carry_no_notification_content() {
        let n = Notification {
            id: "n-1".into(),
            target: "device-1".into(),
            category: "alert".into(),
            title: "Failed pairing attempt".into(),
            body: "Someone tried to pair".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            data: None,
        };
        let msg = wake_message("fcm-token-abc", &n);

        assert_eq!(msg["message"]["token"], "fcm-token-abc");
        assert_eq!(msg["message"]["data"]["notification_id"], "n-1");
        assert_eq!(msg["message"]["data"]["category"], "alert");
        assert_eq!(msg["message"]["data"]["wake"], "1");
        assert_eq!(msg["message"]["android"]["priority"], "HIGH");
        // The content-free guarantee.
        assert!(msg["message"].get("notification").is_none());
        let serialized = msg.to_string();
        assert!(!serialized.contains("Failed pairing attempt"));
        assert!(!serialized.contains("Someone tried to pair"));
    }
}
