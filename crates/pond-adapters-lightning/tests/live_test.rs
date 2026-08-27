//! Real connection to the Breez/Spark testnet — #[ignore]d, same convention
//! as pond-adapters-goose's *_live_test.rs (GIAP_OLLAMA_URL/GIAP_LLAMAFILE_URL).
//!
//! Cannot run yet: needs a real BREEZ_API_KEY, which is still being
//! requested (breez.technology/request-api-key) as of when this was
//! written. Once it exists:
//!
//!   BREEZ_API_KEY=<key> cargo test -p pond-adapters-lightning --test live_test -- --ignored
//!
//! Defaults to Network::Regtest (no real funds) unless LIGHTNING_NETWORK=mainnet
//! is set explicitly.

use pond_adapters_lightning::{LightningConfig, LightningPaymentRail};
use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::ports::payment_rail::PaymentRail;

#[tokio::test]
#[ignore = "requires a real BREEZ_API_KEY (breez.technology/request-api-key)"]
async fn issuing_a_real_invoice_round_trips_through_list_payments() {
    let config = LightningConfig::from_env("/tmp/pond-lightning-live-test".to_string())
        .expect("BREEZ_API_KEY must be set to run this test");
    let (rail, generated_mnemonic) = LightningPaymentRail::connect(config)
        .await
        .expect("failed to connect to Spark network");
    if let Some(mnemonic) = generated_mnemonic {
        // Write to a file instead of logging it — a real wallet seed
        // must never land in a terminal/CI log via --nocapture.
        let path = "/tmp/pond-lightning-live-test/generated-mnemonic.txt";
        std::fs::write(path, &mnemonic).expect("failed to save generated mnemonic to disk");
        eprintln!(
            "generated a fresh wallet mnemonic for this test run — not persisted by \
             the test itself, saved to {path} (delete after copying it into \
             SettingsRepository, if this run is meant to be kept)"
        );
    }

    let invoice = rail
        .issue_invoice(Millisats::new(1000))
        .await
        .expect("issue_invoice should succeed against a live connection");
    assert!(
        invoice.starts_with("ln"),
        "expected a bolt11 invoice string, got: {invoice}"
    );

    // Without actually paying the invoice from a second wallet, no HTLC
    // preimage exists yet — verify_preimage should report false /
    // "not found", never a panic or a false-positive success.
    // Err ("no received payment found yet") is also an acceptable outcome.
    if let Ok(matched) = rail.verify_preimage(&invoice, "0000").await {
        assert!(!matched, "an unpaid invoice must never verify as paid");
    }
}
