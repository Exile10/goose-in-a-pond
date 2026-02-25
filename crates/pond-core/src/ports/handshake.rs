//! Driven Port: Handshake
//!
//! Defines the contract for device authentication between GIAP and
//! connecting clients (GOTG mobile app, other pond instances, etc.)
//!
//! # TODO
//! - [ ] Define full handshake protocol (challenge-response? token exchange?)
//! - [ ] Add token refresh mechanism
//! - [ ] Add device capability negotiation

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Request from a client wanting to connect to GIAP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    /// Client identifier (e.g. mobile device UUID)
    pub client_id: String,
    /// Client type: "gotg", "pond", "cli", etc.
    pub client_type: String,
    /// Client version string
    pub client_version: String,
    /// Optional pre-shared key or pairing code
    pub pairing_code: Option<String>,
}

/// Response from GIAP after a successful handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    /// Whether the handshake succeeded
    pub accepted: bool,
    /// Session token for subsequent API calls
    pub session_token: Option<String>,
    /// GIAP hostname
    pub hostname: String,
    /// GIAP version
    pub server_version: String,
    /// Capabilities this GIAP instance supports
    pub capabilities: Vec<String>,
    /// Reason if rejected
    pub rejection_reason: Option<String>,
}

/// Driven Port: device authentication and pairing.
#[async_trait]
pub trait Handshake: Send + Sync {
    /// Perform a handshake with a connecting client.
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse>;

    /// Validate an existing session token.
    async fn validate_token(&self, token: &str) -> Result<bool>;

    /// Revoke a session token (disconnect a client).
    async fn revoke_token(&self, token: &str) -> Result<()>;
}
