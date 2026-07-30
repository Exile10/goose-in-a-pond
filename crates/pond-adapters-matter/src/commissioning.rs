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
//! Either way the controller answers with the freshly commissioned node. When
//! the user supplied a name it is written to the device's NodeLabel attribute,
//! so the name lives on the device itself and any controller sees it.
//!
//! `decommission` sends `remove_node`, which the delete path uses so a removed
//! device does not re-announce itself on the next `start_listening`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_core::user_data::ports::device_commissioning::{
    CommissionedDevice, DeviceCommissioningPort, SetupCode,
};
use serde_json::json;

use crate::client::MatterClient;
use crate::control::SharedMatterClient;
use crate::protocol::{node_to_device, MatterNode, ATTR_NODE_LABEL, CLUSTER_BASIC_INFORMATION};

/// Commissioning is slow: discovery, attestation, and fabric join, often over
/// a minute on a busy network. Well past the default command timeout.
const COMMISSION_TIMEOUT: Duration = Duration::from_secs(180);

/// Removal is slow too: the controller first tries to unpair from the device,
/// which for an unreachable node waits out an mDNS/CHIP timeout (~15-30s)
/// before removing the node from its own storage. The default 15s command
/// timeout is shorter than that, so GIAP would give up while the server was
/// still finishing — reporting a failure for a removal that actually happened.
const DECOMMISSION_TIMEOUT: Duration = Duration::from_secs(90);

pub struct MatterCommissioner {
    /// The swappable handle, not a fixed connection. Commissioning outlives any
    /// single socket: the supervisor replaces the client after a drop, and a
    /// controller restart is now something a user can ask for — a commissioner
    /// pinned to the original connection would go quietly dead at the first
    /// reconnect and fail every pairing afterwards.
    client: SharedMatterClient,
}

impl MatterCommissioner {
    pub fn new(client: SharedMatterClient) -> Self {
        Self { client }
    }

    /// The connection currently in use, cloned so the lock is released before
    /// any await on the network.
    async fn client(&self) -> Arc<MatterClient> {
        self.client.read().await.clone()
    }

    /// Write the user-chosen name to the device's NodeLabel. Best-effort: if the
    /// write fails the device is still commissioned, so this warns rather than
    /// unwinding a successful pairing — the local registry name still applies.
    async fn set_node_label(&self, node_id: u64, name: &str) {
        let args = json!({
            "node_id": node_id,
            "attribute_path": format!("0/{CLUSTER_BASIC_INFORMATION}/{ATTR_NODE_LABEL}"),
            "value": name,
        });
        if let Err(e) = self
            .client()
            .await
            .send_command("write_attribute", args)
            .await
        {
            tracing::warn!(node_id, error = %e, "matter: could not write NodeLabel");
        }
    }
}

#[async_trait]
impl DeviceCommissioningPort for MatterCommissioner {
    async fn commission(
        &self,
        code: SetupCode,
        name: Option<String>,
    ) -> Result<CommissionedDevice> {
        let (command, args) = match code {
            SetupCode::PairingCode(code) => ("commission_with_code", json!({ "code": code })),
            SetupCode::Passcode(pin) => ("commission_on_network", json!({ "setup_pin_code": pin })),
        };

        let result = self
            .client()
            .await
            .send_command_with_timeout(command, args, COMMISSION_TIMEOUT)
            .await
            .context("commissioning failed")?;

        let node: MatterNode = serde_json::from_value(result)
            .context("the controller did not return a commissioned node")?;
        let device = node_to_device(&node);

        // A user-chosen name is written to the device and used verbatim; the
        // cluster-derived name is the fallback.
        let final_name = match &name {
            Some(n) => {
                self.set_node_label(node.node_id, n).await;
                n.clone()
            }
            None => device.name,
        };

        Ok(CommissionedDevice {
            device_id: device.id,
            name: final_name,
            node_id: node.node_id,
            device_type: device.device_type,
            capabilities: device.capabilities,
        })
    }

    async fn decommission(&self, node_id: u64) -> Result<()> {
        match self
            .client()
            .await
            .send_command_with_timeout(
                "remove_node",
                json!({ "node_id": node_id }),
                DECOMMISSION_TIMEOUT,
            )
            .await
        {
            Ok(_) => Ok(()),
            // A node the controller no longer knows is already in the desired
            // end state, so the delete should proceed rather than be refused —
            // this is what lets an interrupted earlier removal be cleaned up.
            Err(e) if e.to_string().to_lowercase().contains("does not exist") => {
                tracing::info!(node_id, "matter: node already absent from fabric");
                Ok(())
            }
            Err(e) => Err(e).context("removing the node from the fabric failed"),
        }
    }
}
