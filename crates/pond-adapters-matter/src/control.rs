//! [`MatterDeviceControl`] — the [`DeviceControlPort`] over a live
//! matter-server connection. Verbs map onto Matter clusters exactly as proven
//! in the live MVD session (`device_command` with node/endpoint/cluster).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::user_data::ports::device_control::{
    DeviceControlOutcome, DeviceControlPort, DeviceStatePatch,
};
use serde_json::json;
use tokio::sync::RwLock;

use crate::client::MatterClient;
use crate::protocol::{
    brightness_to_level, celsius_to_setpoint, endpoints_with_cluster, node_id_from_device_id,
    MatterNode, ATTR_OCCUPIED_HEATING_SETPOINT, CLUSTER_DOOR_LOCK, CLUSTER_LEVEL_CONTROL,
    CLUSTER_ON_OFF, CLUSTER_THERMOSTAT,
};

/// Shared node cache: the bridge keeps it current from server events; the
/// control port reads it to resolve endpoints per cluster.
pub type NodeCache = Arc<RwLock<HashMap<u64, MatterNode>>>;

pub struct MatterDeviceControl {
    client: Arc<MatterClient>,
    nodes: NodeCache,
}

impl MatterDeviceControl {
    pub fn new(client: Arc<MatterClient>, nodes: NodeCache) -> Self {
        Self { client, nodes }
    }

    /// Resolve a GIAP device id to `(node_id, endpoint)` for `cluster`.
    async fn resolve(&self, device_id: &str, cluster: u32) -> Result<(u64, u16)> {
        let node_id = node_id_from_device_id(device_id).ok_or_else(|| {
            anyhow!(
                "'{device_id}' is not a Matter device id (expected \"matter-<node>\"; \
                 pick the id from the device list)"
            )
        })?;
        let nodes = self.nodes.read().await;
        let node = nodes
            .get(&node_id)
            .ok_or_else(|| anyhow!("Matter node {node_id} is not commissioned on this fabric"))?;
        let endpoint = *endpoints_with_cluster(node, cluster)
            .first()
            .ok_or_else(|| anyhow!("Matter node {node_id} does not support this capability"))?;
        Ok((node_id, endpoint))
    }

    async fn command(
        &self,
        node_id: u64,
        endpoint: u16,
        cluster: u32,
        name: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        self.client
            .send_command(
                "device_command",
                json!({
                    "node_id": node_id,
                    "endpoint_id": endpoint,
                    "cluster_id": cluster,
                    "command_name": name,
                    "payload": payload,
                }),
            )
            .await?;
        Ok(())
    }
}

#[async_trait]
impl DeviceControlPort for MatterDeviceControl {
    async fn set_power(&self, device_id: &str, on: bool) -> Result<DeviceControlOutcome> {
        let (node, ep) = self.resolve(device_id, CLUSTER_ON_OFF).await?;
        self.command(
            node,
            ep,
            CLUSTER_ON_OFF,
            if on { "On" } else { "Off" },
            json!({}),
        )
        .await?;
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                on: Some(on),
                ..Default::default()
            },
        ))
    }

    async fn set_brightness(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        let pct = percent.min(100);
        let (node, ep) = self.resolve(device_id, CLUSTER_LEVEL_CONTROL).await?;
        self.command(
            node,
            ep,
            CLUSTER_LEVEL_CONTROL,
            "MoveToLevelWithOnOff",
            json!({
                "level": brightness_to_level(pct),
                "transitionTime": 0,
                "optionsMask": 0,
                "optionsOverride": 0,
            }),
        )
        .await?;
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                brightness: Some(pct),
                on: Some(pct > 0),
                ..Default::default()
            },
        ))
    }

    async fn set_target_temp(&self, device_id: &str, celsius: f32) -> Result<DeviceControlOutcome> {
        let (node, ep) = self.resolve(device_id, CLUSTER_THERMOSTAT).await?;
        // Setpoints are attribute writes, not commands.
        self.client
            .send_command(
                "write_attribute",
                json!({
                    "node_id": node,
                    "attribute_path": format!("{ep}/{CLUSTER_THERMOSTAT}/{ATTR_OCCUPIED_HEATING_SETPOINT}"),
                    "value": celsius_to_setpoint(celsius),
                }),
            )
            .await?;
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                target_temp: Some(celsius),
                ..Default::default()
            },
        ))
    }

    async fn set_locked(&self, device_id: &str, locked: bool) -> Result<DeviceControlOutcome> {
        let (node, ep) = self.resolve(device_id, CLUSTER_DOOR_LOCK).await?;
        self.command(
            node,
            ep,
            CLUSTER_DOOR_LOCK,
            if locked { "LockDoor" } else { "UnlockDoor" },
            json!({}),
        )
        .await?;
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                locked: Some(locked),
                ..Default::default()
            },
        ))
    }
}
