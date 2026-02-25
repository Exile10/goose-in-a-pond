//! GOTG Handshake Adapter
//!
//! Implements the Handshake port for GOTG mobile app connections.
//!
//! # TODO
//! - [ ] Implement proper challenge-response auth
//! - [ ] Store issued tokens in the system DB
//! - [ ] Add pairing code generation and display on CLI/dashboard
//! - [ ] Add token expiry and refresh
//! - [ ] Rate-limit handshake attempts

use anyhow::Result;
use async_trait::async_trait;
use pond_core::ports::handshake::{Handshake, HandshakeRequest, HandshakeResponse};

/// GOTG-specific handshake adapter.
///
/// Handles authentication for the Goose On The Go mobile app.
pub struct GotgHandshakeAdapter {
    // TODO: Add DB pool, token store, config
}

impl GotgHandshakeAdapter {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Handshake for GotgHandshakeAdapter {
    async fn handshake(&self, request: HandshakeRequest) -> Result<HandshakeResponse> {
        // TODO: Implement real handshake logic
        tracing::info!(
            "Handshake request from {} ({})",
            request.client_id,
            request.client_type
        );

        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        Ok(HandshakeResponse {
            accepted: true, // TODO: validate pairing_code
            session_token: Some(uuid::Uuid::new_v4().to_string()),
            hostname,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec![
                "chat".to_string(),
                "devices".to_string(),
                "settings".to_string(),
            ],
            rejection_reason: None,
        })
    }

    async fn validate_token(&self, _token: &str) -> Result<bool> {
        // TODO: Check token against DB
        Ok(true)
    }

    async fn revoke_token(&self, _token: &str) -> Result<()> {
        // TODO: Remove token from DB
        Ok(())
    }
}
