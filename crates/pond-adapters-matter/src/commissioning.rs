//! [`MatterCommissioner`] — the [`DeviceCommissioningPort`] over a live
//! controller connection.
//!
//! Both setup-code forms go to one `commission` op: the controller decodes the
//! payload and decides how to find the device, which is knowledge that belongs
//! next to matter.js rather than here.
//!
//! `decommission` removes the node from the fabric, which the delete path uses
//! so a removed device does not re-announce itself on the next `subscribe`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_core::user_data::ports::device_commissioning::{
    CommissionedDevice, DeviceCommissioningPort, SetupCode,
};
use serde_json::json;

use crate::client::{code_of, MatterClient};
use crate::notify::MatterNotifier;
use crate::protocol::{
    describe, matter_device_id, matter_node_id, setup_code_kind, CommissionResult, DiscoverResult,
    CODE_NOTHING_PAIRABLE,
};

/// Commissioning is slow: discovery, attestation, and fabric join, often over a
/// minute on a busy network. Well past the default op timeout.
const COMMISSION_TIMEOUT: Duration = Duration::from_secs(180);

/// Removal is slow too: the controller first tries to unpair from the device,
/// which for an unreachable node waits out an mDNS/CHIP timeout (~15-30s) before
/// removing the node from its own storage. The default 15s timeout is shorter
/// than that, so GIAP would give up while the controller was still finishing —
/// reporting a failure for a removal that actually happened.
const DECOMMISSION_TIMEOUT: Duration = Duration::from_secs(90);

/// The pre-flight probe is a local mDNS browse, so it answers in well under a
/// second when anything is advertising. Bounded low on purpose: its whole value
/// is being cheaper than the discovery timeout it saves, and a probe that hangs
/// must not add to the wait.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(15);

/// What the user is told when nothing is advertising itself for pairing. The 15
/// minutes is the Matter commissioning window: a device advertises
/// `_matterc._udp` for roughly that long after it boots and then stops, which
/// makes "it was pairable earlier" the normal way to arrive here.
const NOTHING_IN_PAIRING_MODE: &str =
    "No device found in pairing mode. Put the device into pairing mode and try again — a Matter \
     device stops accepting new connections about 15 minutes after it starts.";

pub struct MatterCommissioner {
    client: Arc<MatterClient>,
    notifier: MatterNotifier,
}

impl MatterCommissioner {
    pub fn new(client: Arc<MatterClient>, notifier: MatterNotifier) -> Self {
        Self { client, notifier }
    }

    /// Refuse early, and legibly, when nothing is in pairing mode.
    ///
    /// Commissioning finds the device over mDNS, so "nothing is advertising"
    /// settles the whole call. Left to the controller it does not look like
    /// that: it waits out a discovery timeout and answers "commissioning
    /// failed", with the actual reason buried in its own output. That is the
    /// most common way commissioning fails, because a device stops advertising
    /// ~15 minutes after it boots, and it is the one failure a user can fix in
    /// ten seconds — if anything tells them what it is.
    ///
    /// A probe that itself fails proves nothing, so it never blocks the attempt:
    /// an unexpected payload or a slow controller falls through to the real
    /// commission rather than inventing a reason to refuse.
    async fn refuse_when_nothing_is_pairable(&self) -> Result<()> {
        let found = match self
            .client
            .send_with_timeout("discover", json!({}), DISCOVER_TIMEOUT)
            .await
        {
            Ok(result) => serde_json::from_value::<DiscoverResult>(result)
                .ok()
                .map(|r| r.commissionable),
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
}

#[async_trait]
impl DeviceCommissioningPort for MatterCommissioner {
    async fn commission(
        &self,
        code: SetupCode,
        name: Option<String>,
    ) -> Result<CommissionedDevice> {
        // Before the discovery wait, not after it: the answer is already known.
        if let Err(e) = self.refuse_when_nothing_is_pairable().await {
            self.notifier.pairing_failed(NOTHING_IN_PAIRING_MODE).await;
            return Err(e);
        }

        // The code itself never reaches a log; only which kind it is.
        let raw = match &code {
            SetupCode::PairingCode(code) => code.clone(),
            SetupCode::Passcode(pin) => pin.to_string(),
        };
        tracing::info!(
            target: "giap::trace",
            kind = "matter_commission_started",
            code_kind = setup_code_kind(&raw),
            named = name.is_some(),
            "matter: commissioning a device"
        );

        let mut params = json!({ "code": raw });
        if let Some(name) = &name {
            params["name"] = json!(name);
        }

        let result = match self
            .client
            .send_with_timeout("commission", params, COMMISSION_TIMEOUT)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                let code = code_of(&e).unwrap_or("none").to_string();
                tracing::warn!(
                    target: "giap::trace",
                    kind = "matter_commission_failed",
                    error_code = %code,
                    error = %describe(&e),
                    "matter: commissioning failed"
                );
                // The controller's own wording, except for the one failure that
                // has better advice than "it failed".
                let told = if code == CODE_NOTHING_PAIRABLE {
                    NOTHING_IN_PAIRING_MODE.to_string()
                } else {
                    describe(&e)
                };
                self.notifier.pairing_failed(&told).await;
                return Err(e).context("commissioning failed");
            }
        };

        let CommissionResult { device } = serde_json::from_value(result)
            .context("the controller did not return a commissioned device")?;
        let node_id = matter_node_id(&device.id).with_context(|| {
            format!(
                "the controller returned '{}', which is not a Matter device id",
                device.id
            )
        })?;

        tracing::info!(
            target: "giap::trace",
            kind = "matter_commission_succeeded",
            device = %device.id,
            device_type = %device.device_type,
            "matter: device joined the fabric"
        );
        // The pairing notification is raised by the bridge when the device
        // reaches the registry, so a device is announced once however it arrived
        // — through this call, or through the `device_added` event that follows.

        Ok(CommissionedDevice {
            device_id: device.id.clone(),
            name: name.unwrap_or_else(|| device.name.clone()),
            node_id,
            device_type: device.device_type.clone(),
            capabilities: device.capabilities.clone(),
        })
    }

    async fn decommission(&self, node_id: u64) -> Result<()> {
        let device_id = matter_device_id(node_id, None);
        // Before the op, not after: the controller's `device_removed` event can
        // reach the bridge while this call is still returning.
        self.notifier.expect_removal(&device_id).await;
        match self
            .client
            .send_with_timeout(
                "decommission",
                json!({ "device_id": device_id }),
                DECOMMISSION_TIMEOUT,
            )
            .await
        {
            Ok(_) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "matter_node_removed",
                    device = %device_id,
                    "matter: node removed from the fabric"
                );
                Ok(())
            }
            Err(e) => Err(e).context("removing the node from the fabric failed"),
        }
    }
}
