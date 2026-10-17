//! Serves the setup channel over this host's BLE peripheral for an unclaimed, in-memory pond
//! (`Pond 7f3a`), so the radio can be checked with btmon and a generic BLE app without changing a
//! real pond's claim. It answers `hello`; the exchange needs a real pond. Stop the real pond
//! first, or a phone sees two setup services on one adapter.
//!
//! ```text
//! cargo run -p pond-connect --example ble_peripheral -- [seconds, default 300]
//! ```

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pond_connect::ble::{self, Advertisement};
use pond_connect::setup_channel::{SetupChannel, SetupChannelConfig, SetupChannelContext};
use pond_core::network::mocks::mock_setup_code_store::MockSetupCodeStore;
use pond_core::network::services::setup_claim::SetupClaimService;
use pond_core::network::services::spoken_code::SpokenCodeService;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;

/// Issues and knows no token: `hello` needs none, and the exchange is a real pond's to check.
struct NoTokens;

#[async_trait]
impl Handshake for NoTokens {
    async fn handshake(&self, _: HandshakeRequest) -> anyhow::Result<HandshakeResponse> {
        anyhow::bail!("this example pond does not pair")
    }
    async fn validate_token(&self, _: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    async fn caller_for_token(&self, _: &str) -> anyhow::Result<Option<TokenCaller>> {
        Ok(None)
    }
    async fn revoke_device(&self, _: &str) -> anyhow::Result<u64> {
        Ok(0)
    }
    async fn revoke_token(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(300);
    let claims = Arc::new(SetupClaimService::new(Arc::new(
        MockSetupCodeStore::unclaimed(),
    )));
    let channel = SetupChannel::new(
        SetupChannelContext::new(
            SetupChannelConfig {
                discriminator: MockSetupCodeStore::DISCRIMINATOR,
                pin: "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
                https_port: 8443,
                hostname: "pond-example".into(),
            },
            Arc::new(NoTokens),
            Arc::new(SpokenCodeService::default()),
            Arc::new(MockSettingsRepository::new()),
            Arc::new(MockProfileRepository::new()),
        )
        .with_claims(claims)
        .with_wifi(pond_connect::wifi::platform()),
    );
    let peripheral = ble::platform();
    if let Err(error) = peripheral.start(Arc::new(channel)).await {
        eprintln!("start: {error}");
    }
    let advertisement = Advertisement {
        local_name: ble::local_name(MockSetupCodeStore::DISCRIMINATOR),
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut last = None;
    while tokio::time::Instant::now() < deadline {
        // Asked again each round, as the server's controller does after a failure.
        let applied = peripheral.advertise(Some(advertisement.clone())).await;
        let status = peripheral.status();
        let line = format!("{status:?} {applied:?}");
        if last.as_ref() != Some(&line) {
            println!("{line}");
            last = Some(line);
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    peripheral.shutdown().await;
    println!("{:?}", peripheral.status());
}
