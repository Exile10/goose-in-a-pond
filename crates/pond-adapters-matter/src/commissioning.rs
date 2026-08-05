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

/// The pre-flight probe is a local mDNS browse, so it answers in well under a
/// second when anything is advertising. Bounded low on purpose: its whole value
/// is being cheaper than the 30s discovery timeout it saves, and a probe that
/// hangs must not add to the wait.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(10);

/// What the user is told when nothing is advertising itself for pairing. The
/// 15 minutes is the Matter commissioning window: a device advertises
/// `_matterc._udp` for roughly that long after it boots and then stops, which
/// makes "it was pairable earlier" the normal way to arrive here.
const NOTHING_IN_PAIRING_MODE: &str =
    "No device found in pairing mode. Put the device into pairing mode and try again — a Matter \
     device stops accepting new connections about 15 minutes after it starts.";

pub struct MatterCommissioner {
    client: Arc<MatterClient>,
}

impl MatterCommissioner {
    pub fn new(client: Arc<MatterClient>) -> Self {
        Self { client }
    }

    /// Refuse early, and legibly, when nothing is in pairing mode.
    ///
    /// Both commissioning commands find the device over mDNS, so "nothing is
    /// advertising" settles the whole call. Left to the controller it does not
    /// look like that: it waits out a 30-second CHIP discovery timeout and
    /// answers "Commissioning failed for node N", with the actual reason
    /// ("Mdns discovery timed out") written only to its own log file on disk.
    /// That is the most common way commissioning fails, because a device stops
    /// advertising ~15 minutes after it boots, and it is the one failure a user
    /// can fix in ten seconds — if anything tells them what it is.
    ///
    /// A probe that itself fails proves nothing, so it never blocks the attempt:
    /// an unexpected payload or a slow controller falls through to the real
    /// commission rather than inventing a reason to refuse.
    async fn refuse_when_nothing_is_pairable(&self) -> Result<()> {
        let found = match self
            .client
            .send_command_with_timeout("discover", json!({}), DISCOVER_TIMEOUT)
            .await
        {
            Ok(result) => result.as_array().map(Vec::len),
            Err(e) => {
                tracing::warn!(error = %e, "matter: could not probe for commissionable devices");
                None
            }
        };

        if found == Some(0) {
            anyhow::bail!(NOTHING_IN_PAIRING_MODE);
        }
        Ok(())
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
        if let Err(e) = self.client.send_command("write_attribute", args).await {
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
        // Before the 30-second wait, not after it: the answer is already known.
        self.refuse_when_nothing_is_pairable().await?;

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
            .client
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
