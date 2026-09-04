//! Lightning settlement for the private mesh (#132) via `breez-sdk-spark`, the nodeless
//! backend with no channel-liquidity management. `issue_invoice`/`verify_preimage` need only
//! the SDK; `batch_settle` takes the peer's invoice as a parameter because this crate has no
//! transport. Obtain it via `pond_adapters_mesh_inference::MeshInferenceService::request_invoice`.

use std::sync::Arc;

use async_trait::async_trait;
use bip39::Mnemonic;
use chrono::Utc;

use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::domain::settlement::SettlementRecord;
use pond_core::mesh::ports::payment_rail::{PaymentRail, PaymentRailError};

use breez_sdk_spark::{
    connect, default_config, BreezSdk, ConnectRequest, ListPaymentsRequest, Network,
    PaymentDetails, PaymentRequest, PaymentType, PrepareSendPaymentRequest, ReceivePaymentMethod,
    ReceivePaymentRequest, Seed, SendPaymentRequest,
};

/// Wallet seed and API key, sourced from env vars only: never hardcoded, never logged.
/// `mnemonic: None` means generate one and hand it back once; the caller must persist it, as
/// `build_mesh_transport` does for `mesh_identity_secret`, because this crate owns no settings.
pub struct LightningConfig {
    pub api_key: String,
    pub mnemonic: Option<String>,
    pub network: Network,
    pub storage_dir: String,
}

impl LightningConfig {
    /// Reads `BREEZ_API_KEY` (required), `LIGHTNING_WALLET_MNEMONIC`
    /// (optional — `None` if unset), and `LIGHTNING_NETWORK` (`"mainnet"` or
    /// default to `Regtest` — the network decision for this first
    /// implementation is testnet/regtest, real funds are not the default).
    pub fn from_env(storage_dir: String) -> anyhow::Result<Self> {
        let api_key = std::env::var("BREEZ_API_KEY")
            .map_err(|_| anyhow::anyhow!("BREEZ_API_KEY is not set"))?;
        let mnemonic = std::env::var("LIGHTNING_WALLET_MNEMONIC").ok();
        let network = match std::env::var("LIGHTNING_NETWORK").as_deref() {
            Ok("mainnet") => Network::Mainnet,
            _ => Network::Regtest,
        };
        Ok(Self {
            api_key,
            mnemonic,
            network,
            storage_dir,
        })
    }
}

pub struct LightningPaymentRail {
    sdk: Arc<BreezSdk>,
}

impl LightningPaymentRail {
    /// Connects to the Spark network. If `config.mnemonic` is `None`, a fresh 24-word BIP-39
    /// mnemonic is generated and returned beside the rail; the caller must persist it (e.g. via
    /// `SettingsRepository::set_key`) or the wallet is unrecoverable after restart.
    pub async fn connect(config: LightningConfig) -> anyhow::Result<(Self, Option<String>)> {
        let (mnemonic, generated) = match config.mnemonic {
            Some(m) => (m, None),
            None => {
                let phrase = Mnemonic::generate(24)
                    .map_err(|e| anyhow::anyhow!("failed to generate wallet mnemonic: {e}"))?
                    .to_string();
                (phrase.clone(), Some(phrase))
            }
        };

        let mut breez_config = default_config(config.network);
        breez_config.api_key = Some(config.api_key);

        let sdk = connect(ConnectRequest {
            config: breez_config,
            seed: Seed::Mnemonic {
                mnemonic,
                passphrase: None,
            },
            storage_dir: config.storage_dir,
        })
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect to Spark network: {e}"))?;

        Ok((Self { sdk: Arc::new(sdk) }, generated))
    }
}

#[async_trait]
impl PaymentRail for LightningPaymentRail {
    async fn issue_invoice(&self, amount: Millisats) -> Result<String, PaymentRailError> {
        // Breez takes whole sats, so rounding Millisats down can undercharge an invoice by up
        // to 999 msat. Acceptable when settling batches of thousands of tokens.
        let amount_sats = amount.value() / 1000;
        let response = self
            .sdk
            .receive_payment(ReceivePaymentRequest {
                payment_method: ReceivePaymentMethod::Bolt11Invoice {
                    description: "GIAP mesh settlement".to_string(),
                    amount_sats: Some(amount_sats),
                    expiry_secs: None,
                    payment_hash: None,
                },
            })
            .await
            .map_err(|e| PaymentRailError::InvalidInvoice(e.to_string()))?;
        Ok(response.payment_request)
    }

    async fn verify_preimage(
        &self,
        invoice: &str,
        preimage: &str,
    ) -> Result<bool, PaymentRailError> {
        // ReceivePaymentResponse carries no payment id/hash to remember, so scan received
        // payments for the one whose embedded invoice matches. Fine at a household's volume.
        let response = self
            .sdk
            .list_payments(ListPaymentsRequest {
                type_filter: Some(vec![PaymentType::Receive]),
                status_filter: None,
                asset_filter: None,
                payment_details_filter: None,
                from_timestamp: None,
                to_timestamp: None,
                offset: None,
                limit: None,
                sort_ascending: None,
            })
            .await
            .map_err(|e| PaymentRailError::InvalidInvoice(e.to_string()))?;

        let matching = response
            .payments
            .iter()
            .find_map(|payment| match &payment.details {
                Some(PaymentDetails::Lightning {
                    invoice: inv,
                    htlc_details,
                    ..
                }) if inv == invoice => Some(htlc_details.preimage.clone()),
                _ => None,
            });

        match matching {
            Some(Some(real_preimage)) => Ok(real_preimage == preimage),
            Some(None) => Ok(false), // found the payment, but no preimage released yet
            None => Err(PaymentRailError::InvalidInvoice(format!(
                "no received payment found for invoice: {invoice}"
            ))),
        }
    }

    /// Pays `invoice`, which the caller must already have obtained from `peer` (this crate has
    /// no mesh transport). `amount` is cross-checked against the invoice before any funds move,
    /// so a stale or mismatched amount fails loudly instead of over- or under-paying.
    async fn batch_settle(
        &self,
        peer: PeerId,
        amount: Millisats,
        invoice: &str,
    ) -> Result<SettlementRecord, PaymentRailError> {
        let expected_sats = amount.value() / 1000;

        let prepare_response = self
            .sdk
            .prepare_send_payment(PrepareSendPaymentRequest {
                payment_request: PaymentRequest::Input {
                    input: invoice.to_string(),
                },
                amount: None,
                token_identifier: None,
                conversion_options: None,
                fee_policy: None,
            })
            .await
            .map_err(|e| PaymentRailError::InvalidInvoice(e.to_string()))?;

        if prepare_response.amount != expected_sats as u128 {
            return Err(PaymentRailError::InvalidInvoice(format!(
                "invoice amount ({} sats) does not match requested settlement amount ({} sats)",
                prepare_response.amount, expected_sats
            )));
        }

        let response = self
            .sdk
            .send_payment(SendPaymentRequest {
                prepare_response,
                options: None,
                idempotency_key: None,
            })
            .await
            .map_err(|e| PaymentRailError::SettlementFailed(e.to_string()))?;

        let preimage = match &response.payment.details {
            Some(PaymentDetails::Lightning { htlc_details, .. }) => {
                htlc_details.preimage.clone().ok_or_else(|| {
                    PaymentRailError::SettlementFailed(
                        "payment completed but no preimage was released yet".to_string(),
                    )
                })?
            }
            _ => {
                return Err(PaymentRailError::SettlementFailed(
                    "payment completed but the response was not a Lightning payment".to_string(),
                ))
            }
        };

        Ok(SettlementRecord {
            peer_id: peer,
            amount,
            preimage,
            settled_at: Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemonic_generation_produces_a_24_word_phrase() {
        let phrase = Mnemonic::generate(24).unwrap().to_string();
        assert_eq!(phrase.split_whitespace().count(), 24);
    }

    /// `std::env` is process-global, so tests touching `BREEZ_API_KEY`/`LIGHTNING_NETWORK`
    /// race under cargo's parallel harness. Hold this lock for the duration of each such test
    /// (same fix as `mic_privacy_integration_test.rs`).
    static ENV_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn config_from_env_requires_api_key() {
        let _lock = ENV_GATE.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialized by ENV_GATE above — no other test in this file
        // reads or writes BREEZ_API_KEY concurrently.
        unsafe {
            std::env::remove_var("BREEZ_API_KEY");
        }
        let result = LightningConfig::from_env("/tmp/pond-lightning-test".to_string());
        assert!(
            result.is_err(),
            "missing BREEZ_API_KEY must be a clear error, not a panic"
        );
    }

    #[test]
    fn config_from_env_defaults_to_regtest() {
        let _lock = ENV_GATE.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialized by ENV_GATE above.
        unsafe {
            std::env::set_var("BREEZ_API_KEY", "test-key");
            std::env::remove_var("LIGHTNING_NETWORK");
        }
        let config = LightningConfig::from_env("/tmp/pond-lightning-test".to_string()).unwrap();
        assert!(matches!(config.network, Network::Regtest));
        unsafe {
            std::env::remove_var("BREEZ_API_KEY");
        }
    }
}
