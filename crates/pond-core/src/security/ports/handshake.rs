//! Driven Port: Handshake
//!
//! Device authentication and pairing between GIAP (server) and connecting
//! clients (the GOTG mobile app, other pond instances, the CLI, …).
//!
//! # Two-phase pairing protocol
//!
//! Pairing proves that the client holds a short-lived **pairing code** that the
//! operator read off this server's CLI/dashboard, without ever sending the code
//! over the wire:
//!
//! 1. `init_handshake(InitRequest) -> ChallengeResponse`
//!    The server mints a random 32-byte challenge bound to `client_id`,
//!    persists it with a short TTL, and returns it (base64) to the client.
//! 2. `verify_handshake(VerifyRequest)`
//!    The client computes `mac = HMAC-SHA256(pairing_code, challenge || client_id)`
//!    and submits it. On success the server consumes the challenge + pairing
//!    code, registers the device, and mints a session+refresh token pair.
//!
//! `refresh` rotates an expiring session token; `revoke_token` disconnects a
//! client. Pairing codes are issued by the server via `issue_pairing_code`
//! (shown on the CLI/dashboard) and are single-use.
//!
//! The legacy single-shot `handshake()` method is retained for the in-memory
//! `MockHandshake` (tests) and for already-paired clients that present a
//! pairing code directly.

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
    /// Client version string.
    pub client_version: String,
    /// Optional pairing code (single-shot path).
    pub pairing_code: Option<String>,
}

/// Response from GIAP after a handshake / refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    /// Whether the handshake succeeded.
    pub accepted: bool,
    /// Session token for subsequent API calls (`Authorization: Bearer …`).
    pub session_token: Option<String>,
    /// Refresh token — populated by the two-phase / refresh paths only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// RFC3339 expiry of the session token. Absent for legacy responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// GIAP hostname.
    pub hostname: String,
    /// GIAP version.
    pub server_version: String,
    /// Capabilities this GIAP instance supports.
    pub capabilities: Vec<String>,
    /// Reason if rejected.
    pub rejection_reason: Option<String>,
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
    /// Hex-encoded `HMAC-SHA256(pairing_code, challenge || client_id)`.
    pub mac: String,
    /// Optional friendly device name to record in the devices table.
    #[serde(default)]
    pub device_name: Option<String>,
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
    /// The household member the device pairing with this code becomes.
    ///
    /// `None` -- the default and the only value any shipped caller produces
    /// today -- pairs an **unattributed** device: registered and usable, and
    /// not any member's phone. See [`Handshake::issue_pairing_code_for`] for
    /// why the member is captured here rather than in [`VerifyRequest`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
}

/// Driven Port: device authentication and pairing.
///
/// New two-phase methods carry default impls that return "unsupported" so
/// lightweight adapters (e.g. `MockHandshake`) only need the three core
/// methods. The SQLite adapter overrides them all.
#[async_trait]
pub trait Handshake: Send + Sync {
    /// Legacy single-shot handshake (client presents a pairing code directly,
    /// or none for the mock). Real clients should prefer `init`/`verify`.
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse>;

    /// Validate an existing session token.
    async fn validate_token(&self, token: &str) -> Result<bool>;

    /// The `client_id` a valid token was issued to, if this adapter can say.
    ///
    /// [`validate_token`](Self::validate_token) answers only yes or no, so a
    /// caller that has authenticated a request still cannot name who made it —
    /// which is why `Principal::token(..)` had nothing to populate it with and
    /// no `Principal` was ever constructed in production.
    ///
    /// Defaulted to `Ok(None)` so adapters that cannot answer (and every
    /// existing implementor) need no change: "I do not know" is a truthful
    /// answer, and an audit entry saying `token:<unknown>` is better than one
    /// naming a client id that was inferred.
    async fn client_id_for_token(&self, _token: &str) -> Result<Option<String>> {
        Ok(None)
    }

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

    /// Issue a single-use pairing code **bound to a household member**, for the
    /// operator to read aloud / type into a client. Returns the plaintext code
    /// (the only place it is visible).
    ///
    /// `profile_id: None` issues an ordinary unattributed code, which is what
    /// [`issue_pairing_code`](Self::issue_pairing_code) does and what every
    /// shipped caller does today.
    ///
    /// # Why the member is captured here and not in [`VerifyRequest`]
    ///
    /// The obvious alternative is for the pairing client to say who it is. That
    /// is the same shape as the live hole PAI-1 P4 closed on
    /// `PUT /sessions/{id}/user`, which took a `profile_id` from the request
    /// body and bound it at `Explicit` strength with no ownership check at all.
    /// It would be worse here: `IdentificationSource::PairedDevice` is the
    /// **strongest** rung of `identity_resolution::resolve` and outranks both
    /// face and explicit, so a client-asserted profile would not merely be
    /// unproven -- it would outrank every proof the pond can actually make.
    ///
    /// A pairing code, by contrast, is minted on the host: both
    /// `handshake_pairing_code` and `handshake_issue_pairing_code` refuse a
    /// non-loopback peer inside the handler. Binding the member at issuance
    /// means the answer to "whose device is this?" comes from somebody standing
    /// at the pond, and the pairing client cannot influence it.
    async fn issue_pairing_code_for(&self, _profile_id: Option<&str>) -> Result<PairingCode> {
        Err(anyhow::anyhow!(
            "pairing-code issuance not supported by this adapter"
        ))
    }

    /// Issue an unattributed single-use pairing code.
    ///
    /// Kept as the zero-argument form because it is what the loopback issuance
    /// route and the startup banner call, and an unattributed pair is the right
    /// default: pairing usually happens before anyone has said who they are.
    /// Adapters implement [`issue_pairing_code_for`](Self::issue_pairing_code_for);
    /// this delegates, so an adapter cannot support one and not the other.
    async fn issue_pairing_code(&self) -> Result<PairingCode> {
        self.issue_pairing_code_for(None).await
    }

    /// The most recently issued, unexpired, unconsumed pairing code, if any.
    /// Used by the loopback dashboard endpoint to re-display the code.
    async fn current_pairing_code(&self) -> Result<Option<PairingCode>> {
        Ok(None)
    }
}
