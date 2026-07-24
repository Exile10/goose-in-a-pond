//! The Matter bridge task: keeps GIAP's view of the fabric current.
//!
//! On start it calls `start_listening`, which returns every commissioned node
//! and subscribes the connection to live updates. Nodes are synced into the
//! [`DeviceRegistry`] (stable `matter-<node_id>` ids) and cached for the
//! control port's endpoint lookups. From then on:
//!
//! - `attribute_updated` on a sensor cluster → [`BusEvent::Sensor`] on the
//!   EventBus — so #92 rules, the activity feed, and notifications react to
//!   Matter sensors exactly like any other sensor source.
//! - node lifecycle events refresh the cache and registry.
//!
//! On connection loss `run_matter_bridge` returns and the caller logs it;
//! v1 requires a server restart to reconnect (the control port shares the
//! same connection, so a transparent reconnect needs a client-swap layer —
//! tracked as a follow-up on #195).

use std::sync::Arc;

use anyhow::{Context, Result};
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::user_data::ports::device_registry::{DeviceRegistry, RegisterDeviceRequest};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::client::{MatterClient, MatterEvent};
use crate::control::NodeCache;
use crate::protocol::{node_to_device, sensor_reading_from_update, MatterNode};

/// Sync one node into the cache + registry (register if new, heartbeat if known).
async fn sync_node(
    node: MatterNode,
    nodes: &NodeCache,
    registry: &Arc<dyn DeviceRegistry + Send + Sync>,
) {
    let device = node_to_device(&node);
    nodes.write().await.insert(node.node_id, node);

    match registry.get_device(&device.id).await {
        Ok(Some(_)) => {
            if let Err(e) = registry.heartbeat(&device.id).await {
                tracing::warn!(device = %device.id, error = %e, "matter: heartbeat failed");
            }
        }
        Ok(None) => {
            let request = RegisterDeviceRequest {
                id: Some(device.id.clone()),
                name: device.name.clone(),
                device_type: device.device_type.clone(),
                hostname: None,
                capabilities: device.capabilities.clone(),
                room: None,
            };
            match registry.register(request).await {
                Ok(_) => {
                    tracing::info!(device = %device.id, name = %device.name, "matter: device registered")
                }
                Err(e) => {
                    tracing::warn!(device = %device.id, error = %e, "matter: registration failed")
                }
            }
        }
        Err(e) => tracing::warn!(device = %device.id, error = %e, "matter: registry lookup failed"),
    }
}

/// Run until the connection drops. `client` must be freshly connected;
/// `events` is its event stream.
pub async fn run_matter_bridge(
    client: Arc<MatterClient>,
    mut events: mpsc::Receiver<MatterEvent>,
    nodes: NodeCache,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    bus: Arc<dyn EventBus>,
) -> Result<()> {
    // Initial sync: start_listening returns all commissioned nodes AND
    // subscribes this connection to subsequent events.
    let initial = client
        .send_command("start_listening", json!({}))
        .await
        .context("matter start_listening failed")?;
    let initial_nodes: Vec<MatterNode> =
        serde_json::from_value(initial).context("unexpected start_listening payload")?;
    tracing::info!(
        count = initial_nodes.len(),
        "matter: fabric nodes discovered"
    );
    for node in initial_nodes {
        sync_node(node, &nodes, &registry).await;
    }

    while let Some(MatterEvent { event, data }) = events.recv().await {
        match event.as_str() {
            "attribute_updated" => {
                // data = [node_id, "endpoint/cluster/attribute", value]
                let (Some(node_id), Some(path), Some(value)) = (
                    data.get(0).and_then(Value::as_u64),
                    data.get(1).and_then(Value::as_str),
                    data.get(2),
                ) else {
                    continue;
                };
                // Keep the cache current so endpoint lookups stay accurate.
                if let Some(node) = nodes.write().await.get_mut(&node_id) {
                    node.attributes.insert(path.to_string(), value.clone());
                }
                if let Some(reading) = sensor_reading_from_update(node_id, path, value) {
                    tracing::debug!(
                        device = %reading.device_id,
                        sensor = %reading.sensor_type,
                        value = reading.value,
                        "matter: sensor update"
                    );
                    bus.publish(BusEvent::Sensor(reading));
                }
            }
            "node_added" | "node_updated" => {
                if let Ok(node) = serde_json::from_value::<MatterNode>(data) {
                    sync_node(node, &nodes, &registry).await;
                }
            }
            "node_removed" => {
                if let Some(node_id) = data.as_u64() {
                    nodes.write().await.remove(&node_id);
                    tracing::info!(node_id, "matter: node removed from fabric");
                }
            }
            _ => {}
        }
    }
    Ok(()) // event channel closed = connection gone; caller reconnects
}
