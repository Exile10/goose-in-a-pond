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
//! On connection loss `run_matter_bridge` returns. [`run_matter_supervisor`]
//! wraps it in a reconnect loop (#195): when the connection drops it
//! re-establishes the WebSocket with backoff, swaps the new client into the
//! shared handle the control port reads, and re-runs the bridge — which calls
//! `start_listening` again and resyncs the fabric. A matter-server restart no
//! longer needs a pond-server restart.
//!
//! Reconnecting only recovers a controller that is up. When the controller
//! *process* has died there is nothing to reconnect to, so after
//! [`RESPAWN_AFTER`] consecutive failures the supervisor re-runs the local
//! controller setup before the next attempt, and keeps doing so on that cadence
//! until it is back. A killed matter-server no longer needs the user to toggle
//! Matter off and on.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::user_data::ports::device_registry::{DeviceRegistry, RegisterDeviceRequest};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::client::{MatterClient, MatterEvent};
use crate::control::{NodeCache, SharedMatterClient};
use crate::protocol::{node_to_device, sensor_reading_from_update, MatterNode};
use crate::server_setup::{revive_local_controller, Revival, SharedServerChild};

/// Reconnect backoff bounds. Exponential from `RECONNECT_BASE` doubling to
/// `RECONNECT_MAX`, with equal jitter so several Ponds pointed at one restarted
/// matter-server don't reconnect in lockstep.
const RECONNECT_BASE: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Consecutive reconnect failures before the supervisor stops assuming the
/// controller is merely unreachable and tries to restart it. Three is past the
/// blip a restarting controller causes (~3.5s of backoff) while still well
/// inside the time a user would wait before reaching for the toggle themselves.
const RESPAWN_AFTER: u32 = 3;

/// How long a revived controller gets to start listening. Shorter than the
/// startup budget: by the time the supervisor runs, the venv already exists, so
/// this waits on a process start rather than on a `pip install`.
const RESPAWN_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the supervisor reconnects to, and what it needs to bring the
/// controller back when reconnecting is not enough.
pub struct SupervisorConfig {
    /// The matter-server WebSocket URL. Also decides whether the controller is
    /// GIAP's to restart: only a loopback URL is.
    pub url: String,
    /// GIAP's data dir — where the controller's venv and fabric storage live.
    pub data_dir: PathBuf,
    /// The controller GIAP started, if any. A respawn replaces the dead handle
    /// here, so the reconciler's teardown still kills the live process.
    pub child: SharedServerChild,
}

/// Should the reconnect about to be made (1-based `attempt`) re-run controller
/// setup first? True on the attempt following every [`RESPAWN_AFTER`] failures,
/// so a controller that stays dead keeps being retried for as long as the
/// outage lasts rather than once and never again. Pure, so the schedule is
/// unit-testable without sleeping.
fn should_respawn_controller(attempt: u32) -> bool {
    attempt > 1 && (attempt - 1).is_multiple_of(RESPAWN_AFTER)
}

/// Backoff delay for reconnect attempt `attempt` (1-based), with equal jitter.
/// Pure so the schedule is unit-testable without sleeping.
fn reconnect_backoff(attempt: u32, rand_unit: f64) -> Duration {
    let base = RECONNECT_BASE.as_millis() as u64;
    let cap = RECONNECT_MAX.as_millis() as u64;
    // Exponential, saturating, capped — shift caps at 63 to avoid overflow.
    let exp = base.saturating_mul(
        1u64.checked_shl(attempt.saturating_sub(1))
            .unwrap_or(u64::MAX),
    );
    let capped = exp.min(cap);
    // Equal jitter: half fixed, half random in [0, half].
    let half = capped / 2;
    let jitter = (half as f64 * rand_unit.clamp(0.0, 1.0)) as u64;
    Duration::from_millis(half + jitter)
}

/// Sync one node into the cache + registry (register if new, heartbeat if known).
async fn sync_node(
    node: MatterNode,
    nodes: &NodeCache,
    registry: &Arc<dyn DeviceRegistry + Send + Sync>,
    bus: &Arc<dyn EventBus>,
) {
    let device = node_to_device(&node);

    // Publish what the node already reports, before waiting on it to change.
    // `start_listening` hands over every current attribute, so a sensor that
    // sits at a steady value is knowable immediately — otherwise it exists in
    // the device list while every question about its reading is answered "none
    // recorded", which reads as "that device is not here".
    //
    // Only what we have not already seen at that value, though. This function
    // runs on the initial sync, on every `node_added`/`node_updated`, and on
    // every supervisor reconnect, and the rules engine (#92) is LEVEL-based:
    // republishing a steady "motion = true" is indistinguishable to it from
    // motion starting again. A controller that reconnects a few times would
    // re-fire every automation attached to every Matter sensor, with nothing in
    // the house having changed — and the reconnect-often case is exactly the
    // one the supervisor above exists to handle.
    //
    // First sight still publishes everything, which is the behaviour the
    // paragraph above is about; the cache is what distinguishes the two.
    let previous = nodes.read().await.get(&node.node_id).cloned();
    for (path, value) in &node.attributes {
        let unchanged = previous
            .as_ref()
            .and_then(|p| p.attributes.get(path))
            .is_some_and(|before| before == value);
        if unchanged {
            continue;
        }
        if let Some(reading) = sensor_reading_from_update(node.node_id, path, value) {
            bus.publish(BusEvent::Sensor(reading));
        }
    }

    nodes.write().await.insert(node.node_id, node);

    match registry.get_device(&device.id).await {
        Ok(Some(existing)) => {
            if let Err(e) = registry.heartbeat(&device.id).await {
                tracing::warn!(device = %device.id, error = %e, "matter: heartbeat failed");
            }
            // Re-derived typing has to reach a device that already exists, or
            // it only ever applies to devices commissioned after the improvement
            // shipped. Registration was the sole writer of these two fields and
            // this arm is the "already registered" one, so every fan and sensor
            // already on the fabric kept `device_type: "matter"` and no
            // capabilities through every restart.
            //
            // Guarded on a real difference because this runs on the initial
            // sync, on every node_added/node_updated, and on every supervisor
            // reconnect — an unconditional UPDATE would be a write per node per
            // reconnect for a value that almost never changes.
            if existing.device_type != device.device_type
                || existing.capabilities != device.capabilities
            {
                if let Err(e) = registry
                    .set_discovered_profile(&device.id, &device.device_type, &device.capabilities)
                    .await
                {
                    tracing::warn!(device = %device.id, error = %e, "matter: profile refresh failed");
                } else {
                    tracing::info!(
                        device = %device.id,
                        from = %existing.device_type,
                        to = %device.device_type,
                        "matter: re-typed an already-registered device"
                    );
                }
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
        sync_node(node, &nodes, &registry, &bus).await;
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
                    sync_node(node, &nodes, &registry, &bus).await;
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

/// Run the bridge forever, reconnecting transparently when the matter-server
/// connection drops (#195).
///
/// Owns the loop that [`run_matter_bridge`] documented as "caller reconnects":
/// on drop it re-establishes the WebSocket with capped, jittered backoff, swaps
/// the fresh client into `client_cell` (so the control port the MCP tool holds
/// keeps working without being rebuilt), and re-runs the bridge — which calls
/// `start_listening` again and resyncs every node into the cache and registry.
///
/// When reconnecting keeps failing it also revives the controller itself — see
/// [`should_respawn_controller`] — because a process that has exited will never
/// answer a reconnect, however long the loop runs.
///
/// `client` / `events` are the already-established initial connection (from the
/// startup connect, which also decided Matter-vs-logging-stub). This never
/// returns while the process lives; it is expected to be `tokio::spawn`ed.
pub async fn run_matter_supervisor(
    config: SupervisorConfig,
    client_cell: SharedMatterClient,
    mut client: Arc<MatterClient>,
    mut events: mpsc::Receiver<MatterEvent>,
    nodes: NodeCache,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    bus: Arc<dyn EventBus>,
) {
    let SupervisorConfig {
        url,
        data_dir,
        child,
    } = config;

    loop {
        match run_matter_bridge(
            client.clone(),
            events,
            nodes.clone(),
            registry.clone(),
            bus.clone(),
        )
        .await
        {
            Ok(()) => tracing::warn!("matter: bridge stopped (connection closed); reconnecting"),
            Err(e) => tracing::warn!(error = %e, "matter: bridge failed; reconnecting"),
        }

        // Reconnect with backoff until it succeeds; the fabric is resynced when
        // the next run_matter_bridge calls start_listening.
        let mut attempt: u32 = 1;
        let (new_client, new_events) = loop {
            // Enough failures in a row means the controller is probably gone
            // rather than busy — reconnecting cannot fix that, restarting can.
            if should_respawn_controller(attempt) {
                match revive_local_controller(&data_dir, &url, &child, RESPAWN_READY_TIMEOUT).await
                {
                    Ok(Revival::Restarted) => tracing::info!(
                        url = %url,
                        "matter: controller was not running; restarted it"
                    ),
                    // Reused: the controller is up, so the fault is in the
                    // connection and the backoff below is the right answer.
                    // NotLocal: another host's controller, not ours to restart.
                    Ok(Revival::Reused | Revival::NotLocal) => {}
                    Err(e) => tracing::warn!(
                        error = %e,
                        "matter: controller restart failed; will retry with the next attempts"
                    ),
                }
            }

            let delay = reconnect_backoff(attempt, rand::random::<f64>());
            tokio::time::sleep(delay).await;
            match MatterClient::connect(&url).await {
                Ok(pair) => break pair,
                Err(e) => {
                    tracing::warn!(
                        url = %url,
                        attempt,
                        error = %e,
                        "matter: reconnect attempt failed; will retry"
                    );
                    attempt = attempt.saturating_add(1);
                }
            }
        };

        *client_cell.write().await = new_client.clone();
        client = new_client;
        events = new_events;
        tracing::info!(url = %url, "matter: reconnected to matter-server");
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps_and_stays_within_jitter_bounds() {
        // With zero jitter the delay is exactly half the (capped) exponential.
        assert_eq!(reconnect_backoff(1, 0.0), Duration::from_millis(500)); // 1s/2
        assert_eq!(reconnect_backoff(2, 0.0), Duration::from_secs(1)); // 2s/2
        assert_eq!(reconnect_backoff(3, 0.0), Duration::from_secs(2)); // 4s/2

        // Caps at RECONNECT_MAX (30s): half = 15s regardless of attempt, and a
        // huge attempt must not overflow.
        assert_eq!(reconnect_backoff(20, 0.0), Duration::from_secs(15));
        assert_eq!(reconnect_backoff(u32::MAX, 0.0), Duration::from_secs(15));

        // Full jitter adds up to another half; a capped attempt lands in
        // [15s, 30s].
        let full = reconnect_backoff(20, 1.0);
        assert!(full >= Duration::from_secs(15) && full <= Duration::from_secs(30));
    }

    #[test]
    fn controller_revival_waits_for_repeated_failures_then_keeps_retrying() {
        // A controller merely restarting is back within a couple of attempts.
        // Reviving on those would race its own startup and, worse, treat every
        // ordinary blip as a dead process.
        assert!(!should_respawn_controller(1));
        assert!(!should_respawn_controller(2));
        assert!(!should_respawn_controller(3));

        // Three failures in a row: try reviving before the fourth attempt.
        assert!(should_respawn_controller(4));

        // Still dead: keep trying on the same cadence rather than giving up
        // after one go, which would leave Matter down for the whole outage.
        assert!(!should_respawn_controller(5));
        assert!(!should_respawn_controller(6));
        assert!(should_respawn_controller(7));
        assert!(should_respawn_controller(10));
    }
}
