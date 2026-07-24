//! [`MatterCommissioner`] — the [`DeviceCommissioningPort`] over a live
//! matter-server connection.
//!
//! The two setup-code forms map onto two different controller commands:
//!
//! - a pairing code / QR payload → `commission_with_code`, which decodes the
//!   payload itself;
//! - a bare passcode → `commission_on_network`, which finds the device by
//!   on-network discovery (how development devices such as Google's Matter
//!   Virtual Device are paired, since they show only a passcode).
//!
//! Either way the controller answers with the freshly commissioned node, which
//! the bridge then registers exactly like any node it discovers — so the device
//! appears in the Devices section without a second, manual registry write.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_core::user_data::ports::device_commissioning::{
    CommissionedDevice, DeviceCommissioningPort, SetupCode,
};
use serde_json::json;

use crate::client::MatterClient;
use crate::protocol::{node_to_device, MatterNode};

/// Commissioning is slow: discovery, attestation, and fabric join, often over
/// a minute on a busy network. Well past the default command timeout.
const COMMISSION_TIMEOUT: Duration = Duration::from_secs(180);

pub struct MatterCommissioner {
    client: Arc<MatterClient>,
}

impl MatterCommissioner {
    pub fn new(client: Arc<MatterClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DeviceCommissioningPort for MatterCommissioner {
    async fn commission(&self, code: SetupCode) -> Result<CommissionedDevice> {
        let (command, args) = match code {
            SetupCode::PairingCode(code) => ("commission_with_code", json!({ "code": code })),
            SetupCode::Passcode(pin) => ("commission_on_network", json!({ "setup_pin_code": pin })),
        };

        let result = self
            .client
            .send_command_with_timeout(command, args, COMMISSION_TIMEOUT)
            .await
            .context("commissioning failed")?;

        let node: MatterNode = serde_json::from_value(result)
            .context("the controller did not return a commissioned node")?;
        let device = node_to_device(&node);
        Ok(CommissionedDevice {
            device_id: device.id,
            name: device.name,
            node_id: node.node_id,
        })
    }
}
