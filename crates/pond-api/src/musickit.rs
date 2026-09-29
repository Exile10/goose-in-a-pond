//! Apple Music developer tokens. The host signs them so the `.p8` key stays out of every page
//! and extension: the player page asks for a token and never sees the key.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::{engine::general_purpose::STANDARD, Engine};
use pond_core::security::ports::secret::SecretRepository;
use serde::Serialize;
use serde_json::json;

use crate::player::json_error;
use crate::AppState;

pub const TEAM_ID_KEY: &str = "APPLE_MUSIC_TEAM_ID";
pub const KEY_ID_KEY: &str = "APPLE_MUSIC_KEY_ID";
pub const PRIVATE_KEY_KEY: &str = "APPLE_MUSIC_PRIVATE_KEY";

/// The player page holds a token for as long as its window lives, and a token that lapses mid-song
/// ends the music; Apple's own ceiling is about six months.
pub const DEVELOPER_TOKEN_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const PEM_BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
const PEM_END: &str = "-----END PRIVATE KEY-----";
const PEM_LINE_WIDTH: usize = 64;

const NOT_SET_UP: &str = "Apple Music is not set up: add your Apple Music Team ID, Key ID and \
                          private key in the Music extension's settings.";
const KEY_NOT_PKCS8: &str = "The Apple Music private key must be the key from the .p8 file \
                             Apple gave you (it starts with -----BEGIN PRIVATE KEY-----).";
const KEY_UNREADABLE: &str = "The Apple Music private key could not be read. Paste the whole \
                              contents of the .p8 file again.";

/// Rebuilds a PKCS#8 PEM from a pasted key in any whitespace form: the UI's single-line input
/// collapses the newlines a PEM needs, and some users paste only the base64 body.
pub fn normalize_private_key(raw: &str) -> Result<String, &'static str> {
    let body: String = raw
        .replace(PEM_BEGIN, "")
        .replace(PEM_END, "")
        .replace("\\n", "")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if body.is_empty() {
        return Err(KEY_UNREADABLE);
    }
    // Standard base64 has no '-', so any left over is another PEM label, e.g. SEC1's.
    if body.contains('-') {
        return Err(KEY_NOT_PKCS8);
    }
    let der = STANDARD
        .decode(body.as_bytes())
        .map_err(|_| KEY_UNREADABLE)?;
    let canonical = STANDARD.encode(der);

    let mut pem = String::with_capacity(canonical.len() + canonical.len() / PEM_LINE_WIDTH + 64);
    pem.push_str(PEM_BEGIN);
    pem.push('\n');
    for line in canonical.as_bytes().chunks(PEM_LINE_WIDTH) {
        // Base64 output is ASCII, so every chunk boundary is a char boundary.
        pem.push_str(std::str::from_utf8(line).map_err(|_| KEY_UNREADABLE)?);
        pem.push('\n');
    }
    pem.push_str(PEM_END);
    pem.push('\n');
    Ok(pem)
}

#[derive(Serialize)]
struct DeveloperClaims<'a> {
    iss: &'a str,
    iat: u64,
    exp: u64,
}

/// The three stored secrets a developer token is signed with.
pub struct SigningCredentials {
    pub team_id: String,
    pub key_id: String,
    pub private_key: String,
}

impl SigningCredentials {
    /// An ES256 developer token issued at `issued_at` (unix seconds), with its `exp`.
    pub fn sign(&self, issued_at: u64, ttl: Duration) -> Result<(String, u64), String> {
        let pem = normalize_private_key(&self.private_key)?;
        let key = jsonwebtoken::EncodingKey::from_ec_pem(pem.as_bytes())
            .map_err(|_| KEY_UNREADABLE.to_string())?;
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.typ = None;
        header.kid = Some(self.key_id.clone());
        let exp = issued_at + ttl.as_secs();
        let claims = DeveloperClaims {
            iss: &self.team_id,
            iat: issued_at,
            exp,
        };
        // A well-formed PKCS#8 of the wrong curve or a corrupt scalar only fails here.
        let token =
            jsonwebtoken::encode(&header, &claims, &key).map_err(|_| KEY_UNREADABLE.to_string())?;
        Ok((token, exp))
    }
}

async fn stored(repo: &dyn SecretRepository, key: &str) -> Option<String> {
    repo.get(key)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

async fn stored_credentials(repo: &dyn SecretRepository) -> Option<SigningCredentials> {
    Some(SigningCredentials {
        team_id: stored(repo, TEAM_ID_KEY).await?,
        key_id: stored(repo, KEY_ID_KEY).await?,
        private_key: stored(repo, PRIVATE_KEY_KEY).await?,
    })
}

/// Whether all three signing secrets are stored, not whether they are valid.
pub async fn has_credentials(repo: &dyn SecretRepository) -> bool {
    stored_credentials(repo).await.is_some()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `GET /api/v1/musickit/developer-token` -- the player page's developer token.
pub async fn developer_token_handler(State(state): State<Arc<AppState>>) -> Response {
    let Some(repo) = &state.secret_repo else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Secret storage not available",
        );
    };
    let Some(credentials) = stored_credentials(repo.as_ref()).await else {
        return json_error(StatusCode::BAD_REQUEST, NOT_SET_UP);
    };
    match credentials.sign(now_secs(), DEVELOPER_TOKEN_TTL) {
        Ok((token, expires_at)) => {
            Json(json!({ "token": token, "expires_at": expires_at })).into_response()
        }
        Err(reason) => json_error(StatusCode::BAD_REQUEST, reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

    /// A fresh P-256 key per call, so no key material is ever committed.
    fn generated_key() -> (Vec<u8>, Vec<u8>) {
        let pair = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING).unwrap();
        let pkcs8 = pair.to_pkcs8v1().unwrap().as_ref().to_vec();
        (pkcs8, pair.public_key().as_ref().to_vec())
    }

    fn pem_with_newlines(der: &[u8]) -> String {
        let body = STANDARD.encode(der);
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        format!("{PEM_BEGIN}\n{}\n{PEM_END}\n", lines.join("\n"))
    }

    fn credentials(private_key: String) -> SigningCredentials {
        SigningCredentials {
            team_id: "TEAMID1234".into(),
            key_id: "KEYID12345".into(),
            private_key,
        }
    }

    #[test]
    fn developer_token_verifies_against_the_public_key_with_apples_header_and_claims() {
        let (der, public) = generated_key();
        let issued_at = 1_790_000_000;
        let (token, exp) = credentials(pem_with_newlines(&der))
            .sign(issued_at, DEVELOPER_TOKEN_TTL)
            .unwrap();
        assert_eq!(exp, issued_at + 7 * 24 * 60 * 60);
        assert!(
            DEVELOPER_TOKEN_TTL < Duration::from_secs(15_777_000),
            "Apple refuses a token that lives past about six months"
        );

        let header = jsonwebtoken::decode_header(&token).unwrap();
        assert_eq!(header.alg, jsonwebtoken::Algorithm::ES256);
        assert_eq!(header.kid.as_deref(), Some("KEYID12345"));

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::ES256);
        validation.validate_exp = false;
        validation.required_spec_claims.clear();
        let decoded = jsonwebtoken::decode::<serde_json::Value>(
            &token,
            &jsonwebtoken::DecodingKey::from_ec_der(&public),
            &validation,
        )
        .expect("signature verifies with the matching public key");
        assert_eq!(decoded.claims["iss"], "TEAMID1234");
        assert_eq!(decoded.claims["iat"], issued_at);
        assert_eq!(decoded.claims["exp"], exp);

        let (_, other_public) = generated_key();
        assert!(
            jsonwebtoken::decode::<serde_json::Value>(
                &token,
                &jsonwebtoken::DecodingKey::from_ec_der(&other_public),
                &validation,
            )
            .is_err(),
            "a different key must not verify"
        );
    }

    #[test]
    fn a_pasted_key_is_accepted_in_every_whitespace_form() {
        let (der, _) = generated_key();
        let canonical = pem_with_newlines(&der);
        let collapsed = canonical.replace('\n', " ");
        let bare_body = STANDARD.encode(&der);
        let escaped = canonical.replace('\n', "\\n");

        for (form, input) in [
            ("PEM with newlines", canonical.as_str()),
            ("newlines collapsed to spaces", collapsed.as_str()),
            ("bare base64 body", bare_body.as_str()),
            ("JSON-escaped newlines", escaped.as_str()),
        ] {
            let pem =
                normalize_private_key(input).unwrap_or_else(|e| panic!("{form} was rejected: {e}"));
            assert_eq!(pem, canonical, "{form} must rebuild the same PEM");
            assert!(pem.lines().all(|l| l.len() <= 64));
            assert!(
                credentials(input.to_string())
                    .sign(1, DEVELOPER_TOKEN_TTL)
                    .is_ok(),
                "{form} must sign"
            );
        }
    }

    #[test]
    fn garbage_and_wrong_key_shapes_are_rejected_with_actionable_text() {
        for garbage in [
            "",
            "   ",
            "not a key!!",
            "-----BEGIN PRIVATE KEY-----\n-----END PRIVATE KEY-----",
        ] {
            assert!(
                normalize_private_key(garbage).is_err(),
                "{garbage:?} accepted"
            );
        }
        assert_eq!(
            normalize_private_key(
                "-----BEGIN EC PRIVATE KEY-----\nAAAA\n-----END EC PRIVATE KEY-----"
            ),
            Err(KEY_NOT_PKCS8)
        );
        // Valid base64 that is not a key normalises, then fails at signing, not with a panic.
        let err = credentials(STANDARD.encode(b"definitely not a key"))
            .sign(1, DEVELOPER_TOKEN_TTL)
            .unwrap_err();
        assert!(err.contains(".p8"), "got: {err}");
    }
}
