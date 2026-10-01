//! Driven port: device pairing and session tokens.
//! Two-phase pairing proves the client holds the single-use pairing code without sending it.
//!
//! # Channel binding
//!
//! A client on pinned TLS folds the pinned key into the MAC ([`VerifyRequest::channel_binding`]);
//! the server recomputes with its own key, so the two agree only on a direct connection:
//!
//! ```text
//! bound   mac = HMAC(code, "goose-pair-client-v1\0" || challenge || \0 || client_id || \0 || spki)
//! unbound mac = HMAC(code, challenge || client_id)
//! ```
//!
//! The server proves itself with [`HandshakeResponse::server_proof`] over the same transcript
//! under `goose-pair-server-v1`, so a wrong pin fails pairing visibly instead of pairing with the
//! wrong pond. Binding is optional only for the loopback dashboard; it can't be downgraded in
//! transit, since both forms need the pairing code.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Request from a client wanting to connect to GIAP (legacy single-shot).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    /// Client identifier (e.g. mobile device UUID).
    pub client_id: String,
    /// Client type: "gotg", "pond", "cli", etc.
    pub client_type: String,
    pub client_version: String,
    pub pairing_code: Option<String>,
}

/// Response from GIAP after a handshake / refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub accepted: bool,
    /// Session token for subsequent API calls (`Authorization: Bearer …`).
    pub session_token: Option<String>,
    /// Refresh token — populated by the two-phase / refresh paths only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// RFC3339 expiry of the session token. Absent for legacy responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub hostname: String,
    pub server_version: String,
    pub capabilities: Vec<String>,
    pub rejection_reason: Option<String>,
    /// Hex `HMAC-SHA256` proving this server holds the pairing code and serves
    /// the certificate the client pinned. See the channel-binding note above.
    ///
    /// Present exactly when the accepted request carried a
    /// [`VerifyRequest::channel_binding`]. A client that sent one and did not
    /// get one back is not talking to the pond it thinks it is, and must treat
    /// the pair as failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_proof: Option<String>,
}

/// Phase 1 request: the client asks for a challenge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitRequest {
    pub client_id: String,
    pub client_type: String,
    pub client_version: String,
}

/// Phase 1 response: the challenge the client must MAC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeResponse {
    pub challenge_id: String,
    /// Base64-encoded random challenge bytes.
    pub challenge: String,
    /// RFC3339 expiry of the challenge.
    pub expires_at: String,
}

/// Phase 2 request: the client proves possession of the pairing code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    pub challenge_id: String,
    /// Hex-encoded MAC over the transcript named by `channel_binding`.
    pub mac: String,
    /// Optional friendly device name to record in the devices table.
    #[serde(default)]
    pub device_name: Option<String>,
    /// The server public-key pin this client pinned its connection to, in
    /// `sha256/<base64>` form, when it reached the server over pinned TLS.
    ///
    /// `None` means there was no channel to bind -- the loopback dashboard --
    /// and selects the unbound MAC. Anything else must equal this server's own
    /// pin, or the request is rejected: see the channel-binding note above.
    #[serde(default)]
    pub channel_binding: Option<String>,
}

/// Exchange a refresh token for a fresh session+refresh pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// A server-issued pairing code, shown on the CLI/dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingCode {
    /// 6-digit code shown to the operator (leading zeros preserved).
    pub code: String,
    /// RFC3339 expiry of the code.
    pub expires_at: String,
    /// Member the device paired with this code becomes; `None` pairs an unattributed device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

/// Who a valid session token was issued to.
/// The ids match today, but identity must follow `device_id`: `client_id` is self-reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCaller {
    pub client_id: String,
    /// The `devices` row this token's pairing registered.
    pub device_id: String,
}

/// Driven port: device authentication and pairing.
#[async_trait]
pub trait Handshake: Send + Sync {
    /// Legacy single-shot handshake; real clients should prefer `init`/`verify`.
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse>;

    /// Validate an existing session token.
    async fn validate_token(&self, token: &str) -> Result<bool>;

    /// The client and device a valid token was issued to, if this adapter can say.
    /// Sole source of the paired-device identity rung, which outranks face and explicit, so the
    /// device id must never come from request input. The `Ok(None)` default only skips that rung.
    async fn caller_for_token(&self, _token: &str) -> Result<Option<TokenCaller>> {
        Ok(None)
    }

    /// The `client_id` a valid token was issued to, if known; derived so the two cannot disagree.
    async fn client_id_for_token(&self, token: &str) -> Result<Option<String>> {
        Ok(self
            .caller_for_token(token)
            .await?
            .map(|caller| caller.client_id))
    }

    /// Revoke every session a device holds, and report how many were live.
    ///
    /// Removing a device from the household registry has to take its access
    /// with it. `session_tokens.device_id` carries no foreign key, and nothing
    /// cascades onto that table, so deleting the `devices` row on its own left
    /// the tokens valid -- an operator who removed a lost phone from the device
    /// list would have been told it was gone while it carried on working.
    ///
    /// # Why this has no default
    ///
    /// Every other new method on this trait is defaulted, and each of those
    /// defaults **narrows**: a forgotten override loses a capability. A default
    /// here would do the opposite. `Ok(0)` would mean "revoked nothing", the
    /// caller would delete the device row anyway, and the omission would widen
    /// access while reading like success. So it is required, and an adapter
    /// that cannot revoke has to say so out loud.
    async fn revoke_device(&self, device_id: &str) -> Result<u64>;

    /// Revoke a session token (disconnect a client).
    async fn revoke_token(&self, token: &str) -> Result<()>;

    /// Phase 1: issue a challenge bound to a `client_id`.
    async fn init_handshake(&self, _request: InitRequest) -> Result<ChallengeResponse> {
        Err(anyhow::anyhow!(
            "two-phase handshake not supported by this adapter"
        ))
    }

    /// Phase 2: verify the client's MAC and mint tokens.
    async fn verify_handshake(&self, _request: VerifyRequest) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!(
            "two-phase handshake not supported by this adapter"
        ))
    }

    /// Exchange a refresh token for a fresh session+refresh pair.
    async fn refresh(&self, _request: RefreshRequest) -> Result<HandshakeResponse> {
        Err(anyhow::anyhow!("refresh not supported by this adapter"))
    }

    /// Issue a single-use pairing code bound to a member; returns the only plaintext copy.
    /// Bound on the host, never by the pairing client: the paired-device rung outranks all proofs.
    async fn issue_pairing_code_for(&self, _profile_id: Option<&str>) -> Result<PairingCode> {
        Err(anyhow::anyhow!(
            "pairing-code issuance not supported by this adapter"
        ))
    }

    /// Issue an unattributed single-use pairing code; adapters implement `issue_pairing_code_for`.
    async fn issue_pairing_code(&self) -> Result<PairingCode> {
        self.issue_pairing_code_for(None).await
    }

    /// The latest unexpired, unconsumed pairing code, if any, for the dashboard to re-display.
    async fn current_pairing_code(&self) -> Result<Option<PairingCode>> {
        Ok(None)
    }
}
