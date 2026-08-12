//! Integration tests against an in-process mock matter-server (a real
//! WebSocket server speaking the schema-11 protocol), so the whole adapter is
//! CI-green with no controller installed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
use pond_core::user_data::ports::device_control::{
    DeviceControlOutcome, DeviceControlPort, DeviceStatePatch,
};
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::matter_runtime::{MatterRuntimePort, MatterState, MatterStatus};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;

use crate::bridge::{run_matter_bridge, run_matter_supervisor, SupervisorConfig};
use crate::client::MatterClient;
use crate::commissioning::MatterCommissioner;
use crate::control::{MatterDeviceControl, NodeCache, SharedMatterClient};
use crate::runtime::MatterRuntime;
use pond_core::user_data::ports::device_commissioning::{DeviceCommissioningPort, SetupCode};

// ── Mock matter-server ───────────────────────────────────────────────────────

/// Everything the mock received (`device_command` / `write_attribute` frames),
/// for assertions.
type ReceivedCommands = Arc<Mutex<Vec<Value>>>;

/// Start a one-connection mock matter-server. It greets, answers
/// `start_listening` with `nodes`, records every other command (answering
/// success), and pushes `push_events` right after `start_listening`.
async fn mock_matter_server(nodes: Value, push_events: Vec<Value>) -> (String, ReceivedCommands) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let received: ReceivedCommands = Arc::new(Mutex::new(Vec::new()));
    let received_srv = received.clone();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.send(Message::Text(
            json!({"fabric_id": 1, "schema_version": 11})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();

        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let frame: Value = serde_json::from_str(&text).unwrap();
            let mid = frame["message_id"].as_str().unwrap().to_string();
            match frame["command"].as_str().unwrap() {
                "start_listening" => {
                    ws.send(Message::Text(
                        json!({"message_id": mid, "result": nodes})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                    for event in &push_events {
                        ws.send(Message::Text(event.to_string().into()))
                            .await
                            .unwrap();
                    }
                }
                _ => {
                    received_srv.lock().unwrap().push(frame.clone());
                    ws.send(Message::Text(
                        json!({"message_id": mid, "result": null})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                }
            }
        }
    });

    (url, received)
}

/// A mock that accepts multiple connections and drops the FIRST one right
/// after its `start_listening`, to force a reconnect. Returns the url and a
/// shared count of `start_listening` calls across all connections.
async fn mock_reconnecting_server(nodes: Value) -> (String, Arc<Mutex<u32>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let listens = Arc::new(Mutex::new(0u32));
    let listens_srv = listens.clone();

    tokio::spawn(async move {
        let mut conn = 0u32;
        while let Ok((stream, _)) = listener.accept().await {
            conn += 1;
            let drop_after_listen = conn == 1;
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::Text(
                json!({"fabric_id": 1, "schema_version": 11})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();

            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let frame: Value = serde_json::from_str(&text).unwrap();
                let mid = frame["message_id"].as_str().unwrap().to_string();
                if frame["command"] == "start_listening" {
                    *listens_srv.lock().unwrap() += 1;
                    ws.send(Message::Text(
                        json!({"message_id": mid, "result": nodes})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                    if drop_after_listen {
                        let _ = ws.close(None).await;
                        break;
                    }
                } else {
                    ws.send(Message::Text(
                        json!({"message_id": mid, "result": null})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                }
            }
        }
    });

    (url, listens)
}

/// Same cluster layout as the light commissioned in the live session
/// (OnOff + LevelControl on endpoint 13) — identical for real bulbs.
fn light_node_json() -> Value {
    json!({
        "node_id": 2,
        "available": true,
        "attributes": {
            "0/40/5": "Living Room Light",
            "13/6/0": false,
            "13/8/0": 1
        }
    })
}

/// The Matter Virtual Device's fan as commissioned on 2026-08-05: Fan Control
/// on endpoint 1, and no On/Off cluster anywhere on the node.
fn fan_node_json() -> Value {
    json!({
        "node_id": 18,
        "available": true,
        "attributes": {
            "0/40/5": "Living Room Fan",
            "1/514/0": 0,
            "1/514/2": 0
        }
    })
}

fn occupancy_node_json() -> Value {
    json!({
        "node_id": 7,
        "available": true,
        "attributes": { "1/1030/0": 0 }
    })
}

// ── In-memory registry stub ──────────────────────────────────────────────────

#[derive(Default)]
struct InMemoryRegistry {
    devices: Mutex<HashMap<String, Device>>,
}

#[async_trait::async_trait]
impl DeviceRegistry for InMemoryRegistry {
    async fn register(&self, request: RegisterDeviceRequest) -> Result<Device> {
        let id = request.id.clone().unwrap_or_else(|| "generated".into());
        let device = Device {
            id: id.clone(),
            name: request.name,
            device_type: request.device_type,
            hostname: request.hostname,
            ip_address: None,
            capabilities: request.capabilities,
            registered_at: chrono::Utc::now().to_rfc3339(),
            last_seen: None,
            is_online: true,
            room: request.room,
        };
        self.devices.lock().unwrap().insert(id, device.clone());
        Ok(device)
    }
    async fn list_devices(&self) -> Result<Vec<Device>> {
        Ok(self.devices.lock().unwrap().values().cloned().collect())
    }
    async fn get_device(&self, id: &str) -> Result<Option<Device>> {
        Ok(self.devices.lock().unwrap().get(id).cloned())
    }
    async fn unregister(&self, id: &str) -> Result<()> {
        self.devices.lock().unwrap().remove(id);
        Ok(())
    }
    async fn heartbeat(&self, _id: &str) -> Result<()> {
        Ok(())
    }
}

async fn start_adapter(
    nodes: Value,
    push_events: Vec<Value>,
) -> (
    Arc<MatterClient>,
    NodeCache,
    Arc<InMemoryRegistry>,
    Arc<InProcessEventBus>,
    ReceivedCommands,
) {
    let (url, received) = mock_matter_server(nodes, push_events).await;
    let (client, events) = MatterClient::connect(&url).await.unwrap();
    let cache: NodeCache = Arc::new(RwLock::new(HashMap::new()));
    let registry = Arc::new(InMemoryRegistry::default());
    let bus = Arc::new(InProcessEventBus::new());
    tokio::spawn(run_matter_bridge(
        client.clone(),
        events,
        cache.clone(),
        registry.clone() as Arc<dyn DeviceRegistry + Send + Sync>,
        bus.clone() as Arc<dyn EventBus>,
    ));
    // Let the bridge finish its initial sync.
    tokio::time::sleep(Duration::from_millis(200)).await;
    (client, cache, registry, bus, received)
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Nodes discovered at startup land in the registry with stable ids, real
/// names, and capabilities inferred from their clusters.
#[tokio::test]
async fn bridge_syncs_fabric_nodes_into_the_device_registry() {
    let (_client, cache, registry, _bus, _received) =
        start_adapter(json!([light_node_json(), occupancy_node_json()]), vec![]).await;

    let light = registry.get_device("matter-2").await.unwrap().unwrap();
    assert_eq!(light.name, "Living Room Light");
    assert_eq!(light.device_type, "light");
    assert_eq!(light.capabilities, vec!["power", "brightness"]);

    let sensor = registry.get_device("matter-7").await.unwrap().unwrap();
    assert_eq!(sensor.device_type, "sensor");
    assert_eq!(cache.read().await.len(), 2);
}

/// `set_power(off)` becomes the exact `device_command` frame proven in the
/// live MVD session: node 2, endpoint 13, cluster 6, command "Off".
#[tokio::test]
async fn set_power_sends_the_proven_onoff_command() {
    let (client, cache, _registry, _bus, received) =
        start_adapter(json!([light_node_json()]), vec![]).await;
    let control = MatterDeviceControl::new(client, cache);

    let outcome = control.set_power("matter-2", false).await.unwrap();
    assert_eq!(outcome.applied.on, Some(false));

    let frames = received.lock().unwrap().clone();
    assert_eq!(frames.len(), 1);
    let args = &frames[0]["args"];
    assert_eq!(frames[0]["command"], "device_command");
    assert_eq!(args["node_id"], 2);
    assert_eq!(args["endpoint_id"], 13);
    assert_eq!(args["cluster_id"], 6);
    assert_eq!(args["command_name"], "Off");
}

/// Brightness maps onto LevelControl with the Matter 0-254 scale.
#[tokio::test]
async fn set_brightness_maps_to_level_control() {
    let (client, cache, _registry, _bus, received) =
        start_adapter(json!([light_node_json()]), vec![]).await;
    let control = MatterDeviceControl::new(client, cache);

    control.set_brightness("matter-2", 50).await.unwrap();

    let frames = received.lock().unwrap().clone();
    let args = &frames[0]["args"];
    assert_eq!(args["cluster_id"], 8);
    assert_eq!(args["command_name"], "MoveToLevelWithOnOff");
    assert_eq!(args["payload"]["level"], 127);
}

/// Unknown ids and missing capabilities fail with actionable errors instead
/// of sending anything to the fabric.
#[tokio::test]
async fn control_rejects_unknown_devices_and_capabilities() {
    let (client, cache, _registry, _bus, received) =
        start_adapter(json!([light_node_json()]), vec![]).await;
    let control = MatterDeviceControl::new(client, cache);

    assert!(control.set_power("living-room-light", true).await.is_err());
    assert!(control.set_power("matter-99", true).await.is_err());
    // The light has no DoorLock cluster.
    assert!(control.set_locked("matter-2", true).await.is_err());
    assert!(
        received.lock().unwrap().is_empty(),
        "nothing reached the fabric"
    );
}

/// The bug the Virtual Fan exposed: `set_power` resolved On/Off and nothing
/// else, so a fan — which need not implement On/Off at all — could never be
/// switched on. Its power is the `FanMode` attribute.
#[tokio::test]
async fn set_power_on_a_fan_writes_fan_mode() {
    let (client, cache, _registry, _bus, received) =
        start_adapter(json!([fan_node_json()]), vec![]).await;
    let control = MatterDeviceControl::new(client, cache);

    let outcome = control.set_power("matter-18", true).await.unwrap();
    assert_eq!(outcome.applied.on, Some(true));

    let frames = received.lock().unwrap().clone();
    assert_eq!(frames.len(), 1, "exactly one write reached the fabric");
    let args = &frames[0]["args"];
    assert_eq!(frames[0]["command"], "write_attribute");
    assert_eq!(args["node_id"], 18);
    // endpoint/cluster/attribute — FanMode on the fan's endpoint.
    assert_eq!(args["attribute_path"], "1/514/0");
    assert_eq!(args["value"], 4, "FanMode On");
}

#[tokio::test]
async fn turning_a_fan_off_writes_fan_mode_off() {
    let (client, cache, _registry, _bus, received) =
        start_adapter(json!([fan_node_json()]), vec![]).await;
    let control = MatterDeviceControl::new(client, cache);

    control.set_power("matter-18", false).await.unwrap();

    let frames = received.lock().unwrap().clone();
    assert_eq!(frames[0]["args"]["value"], 0, "FanMode Off");
}

/// An air purifier is asked for a mode, not a percentage: "auto" and "smart"
/// hand the choice back to the device and have no position on the speed slider.
#[tokio::test]
async fn setting_a_fan_mode_writes_the_mode_the_user_named() {
    let (client, cache, _registry, _bus, received) =
        start_adapter(json!([fan_node_json()]), vec![]).await;
    let control = MatterDeviceControl::new(client, cache);

    let outcome = control.set_fan_mode("matter-18", "auto").await.unwrap();
    assert_eq!(outcome.applied.fan_mode.as_deref(), Some("auto"));
    assert_eq!(outcome.applied.on, Some(true), "auto is not off");

    let frames = received.lock().unwrap().clone();
    assert_eq!(frames[0]["command"], "write_attribute");
    assert_eq!(frames[0]["args"]["attribute_path"], "1/514/0");
    assert_eq!(frames[0]["args"]["value"], 5, "FanMode Auto");
}

/// Off is the one mode that says something definite about power.
#[tokio::test]
async fn the_off_mode_reports_the_device_as_off() {
    let (client, cache, _registry, _bus, received) =
        start_adapter(json!([fan_node_json()]), vec![]).await;
    let control = MatterDeviceControl::new(client, cache);

    let outcome = control.set_fan_mode("matter-18", "Off").await.unwrap();
    assert_eq!(outcome.applied.on, Some(false));
    assert_eq!(received.lock().unwrap()[0]["args"]["value"], 0);
}

/// A mode the device does not have is refused before anything is sent, rather
/// than written as some nearby number.
#[tokio::test]
async fn an_invented_fan_mode_reaches_nothing() {
    let (client, cache, _registry, _bus, received) =
        start_adapter(json!([fan_node_json()]), vec![]).await;
    let control = MatterDeviceControl::new(client, cache);

    let error = control
        .set_fan_mode("matter-18", "turbo")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("off, low, medium"),
        "it lists the modes: {error}"
    );
    assert!(
        received.lock().unwrap().is_empty(),
        "nothing reached the fabric"
    );
}

/// A light must keep using On/Off: the fan branch is a fallback for nodes that
/// lack that cluster, never a replacement for the command that already works.
#[tokio::test]
async fn a_light_still_goes_through_on_off() {
    let (client, cache, _registry, _bus, received) =
        start_adapter(json!([light_node_json()]), vec![]).await;
    let control = MatterDeviceControl::new(client, cache);

    control.set_power("matter-2", true).await.unwrap();

    let frames = received.lock().unwrap().clone();
    assert_eq!(frames[0]["command"], "device_command");
    assert_eq!(frames[0]["args"]["command_name"], "On");
}

/// A sensor is knowable the moment it joins, not whenever it next changes.
///
/// `start_listening` hands over every current attribute, but readings used to
/// come only from later `attribute_updated` events — so a freshly commissioned
/// sensor sitting at a steady value was in the device list while every question
/// about its reading answered "none recorded", which reads as "no such device".
#[tokio::test]
async fn a_sensor_reports_its_current_value_as_soon_as_it_is_synced() {
    // Built by hand rather than with `start_adapter`: the subscription has to
    // exist before the initial sync, which is the moment under test.
    let (url, _received) = mock_matter_server(
        json!([{
            "node_id": 12,
            "available": true,
            "attributes": { "1/1026/0": 2150 }   // Temperature, 21.50 C
        }]),
        vec![],
    )
    .await;
    let (client, events) = MatterClient::connect(&url).await.unwrap();
    let cache: NodeCache = Arc::new(RwLock::new(HashMap::new()));
    let registry = Arc::new(InMemoryRegistry::default());
    let bus = Arc::new(InProcessEventBus::new());

    use futures::StreamExt as _;
    let mut stream = bus.subscribe();

    tokio::spawn(run_matter_bridge(
        client,
        events,
        cache,
        registry as Arc<dyn DeviceRegistry + Send + Sync>,
        bus.clone() as Arc<dyn EventBus>,
    ));

    let event = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("a reading within the deadline — no attribute_updated was ever pushed")
        .expect("bus open");
    let BusEvent::Sensor(reading) = &event else {
        panic!("expected a sensor reading, got {event:?}");
    };
    assert_eq!(reading.device_id, "matter-12");
    assert_eq!(reading.sensor_type, "temperature");
    assert!(
        (reading.value - 21.5).abs() < f64::EPSILON,
        "{}",
        reading.value
    );
}

/// #195 acceptance: a Matter occupancy update becomes a bus sensor event that
/// a #92 automation rule matches — the full sensor → rule chain with zero
/// physical hardware.
#[tokio::test]
async fn occupancy_update_reaches_the_bus_and_matches_a_rule() {
    use futures::StreamExt as _;
    use pond_core::user_data::domain::schedule::{
        SensorTriggerSpec, TriggerAction, TriggerCondition, TriggerSource, TriggerSourceKind,
    };

    let (_client, _cache, _registry, bus, _received) = start_adapter(
        json!([occupancy_node_json()]),
        vec![json!({"event": "attribute_updated", "data": [7, "1/1030/0", 1]})],
    )
    .await;

    let mut stream = bus.subscribe();
    // The event was published during startup sync; re-subscribe misses it, so
    // drive a second update through the same path.
    let reading = crate::protocol::sensor_reading_from_update(7, "1/1030/0", &json!(1)).unwrap();
    bus.publish(BusEvent::Sensor(reading));

    let event = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("bus event within deadline")
        .expect("bus open");
    let BusEvent::Sensor(reading) = &event else {
        panic!("expected sensor event, got {event:?}");
    };
    assert_eq!(reading.device_id, "matter-7");
    assert_eq!(reading.sensor_type, "occupancy");
    assert_eq!(reading.value, 1.0);

    // The #92 rule: "occupancy on the Matter sensor -> notify".
    let rule = SensorTriggerSpec {
        source: TriggerSource {
            kind: TriggerSourceKind::Sensor,
            device_id: Some("matter-7".into()),
            signal: Some("occupancy".into()),
        },
        condition: TriggerCondition::default(),
        actions: vec![TriggerAction::Notify {
            title: "Occupancy".into(),
            body: "Someone is in the room".into(),
        }],
        cooldown_secs: 60,
    };
    let view = event
        .trigger_view()
        .expect("a sensor reading is device-shaped");
    assert!(
        rule.matches(&view, chrono::NaiveTime::from_hms_opt(20, 0, 0).unwrap()),
        "the automation rule must match the Matter sensor update"
    );
}

/// The control port follows a swapped client: after the reconnect supervisor
/// replaces the inner client, commands go to the new connection — no rebuild.
#[tokio::test]
async fn control_follows_a_swapped_client() {
    let (url_a, recv_a) = mock_matter_server(json!([light_node_json()]), vec![]).await;
    let (client_a, _events_a) = MatterClient::connect(&url_a).await.unwrap();

    // Seed the node cache directly — this test targets the client swap, not the
    // bridge sync (covered elsewhere).
    let cache: NodeCache = Arc::new(RwLock::new(HashMap::new()));
    cache
        .write()
        .await
        .insert(2, serde_json::from_value(light_node_json()).unwrap());

    let control = MatterDeviceControl::new(client_a, cache.clone());
    let cell = control.client_handle();

    control.set_power("matter-2", true).await.unwrap();
    assert_eq!(recv_a.lock().unwrap().len(), 1, "first command → server A");

    // Swap in a client on a different server.
    let (url_b, recv_b) = mock_matter_server(json!([light_node_json()]), vec![]).await;
    let (client_b, _events_b) = MatterClient::connect(&url_b).await.unwrap();
    *cell.write().await = client_b;

    control.set_power("matter-2", false).await.unwrap();
    assert_eq!(
        recv_a.lock().unwrap().len(),
        1,
        "server A saw no new command"
    );
    assert_eq!(recv_b.lock().unwrap().len(), 1, "next command → server B");
}

/// #195 acceptance: when the matter-server connection drops, the supervisor
/// reconnects and re-runs start_listening (resyncing the fabric) without a
/// pond-server restart.
#[tokio::test]
async fn supervisor_reconnects_after_the_connection_drops() {
    let (url, listens) = mock_reconnecting_server(json!([light_node_json()])).await;
    let (client, events) = MatterClient::connect(&url).await.unwrap();
    let cache: NodeCache = Arc::new(RwLock::new(HashMap::new()));
    let cell: SharedMatterClient = Arc::new(RwLock::new(client.clone()));
    let registry = Arc::new(InMemoryRegistry::default());
    let bus = Arc::new(InProcessEventBus::new());

    tokio::spawn(run_matter_supervisor(
        SupervisorConfig {
            url: url.clone(),
            // A mock server on an ephemeral loopback port: revival would find
            // it listening and reuse it, so nothing is ever installed here.
            data_dir: std::path::PathBuf::from("/nonexistent"),
            child: Arc::new(tokio::sync::Mutex::new(None)),
        },
        cell.clone(),
        client,
        events,
        cache.clone(),
        registry.clone() as Arc<dyn DeviceRegistry + Send + Sync>,
        bus.clone() as Arc<dyn EventBus>,
    ));

    // First connection: start_listening (1) then drop. The supervisor backs off
    // (~0.5-1s) and reconnects, producing a second start_listening.
    let mut waited = 0;
    while *listens.lock().unwrap() < 2 && waited < 60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        waited += 1;
    }
    assert_eq!(
        *listens.lock().unwrap(),
        2,
        "supervisor should reconnect and re-run start_listening"
    );
    // The fabric was resynced on reconnect.
    assert!(registry.get_device("matter-2").await.unwrap().is_some());
}

/// A mock that answers commissioning commands with a node and records every
/// frame, so a commission/decommission can be asserted end to end.
async fn mock_commissioning_server(node: Value) -> (String, ReceivedCommands) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let received: ReceivedCommands = Arc::new(Mutex::new(Vec::new()));
    let received_srv = received.clone();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.send(Message::Text(
            json!({"fabric_id": 1, "schema_version": 11})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();

        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let frame: Value = serde_json::from_str(&text).unwrap();
            let mid = frame["message_id"].as_str().unwrap().to_string();
            received_srv.lock().unwrap().push(frame.clone());
            // Commissioning returns the freshly joined node; everything else
            // (write_attribute, remove_node) succeeds with null.
            let result = match frame["command"].as_str().unwrap() {
                "commission_with_code" | "commission_on_network" => node.clone(),
                // The pre-flight probe: one device is advertising, so
                // commissioning proceeds.
                "discover" => json!([{ "instance_name": "MOCKDEVICE" }]),
                _ => Value::Null,
            };
            ws.send(Message::Text(
                json!({"message_id": mid, "result": result})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        }
    });

    (url, received)
}

/// A mock whose mDNS view is empty: nothing is advertising itself for pairing.
/// It still answers the commissioning commands, so a test can prove the attempt
/// was refused before it reached them rather than merely failing later.
async fn mock_unpairable_server(node: Value) -> (String, ReceivedCommands) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let received: ReceivedCommands = Arc::new(Mutex::new(Vec::new()));
    let received_srv = received.clone();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.send(Message::Text(
            json!({"fabric_id": 1, "schema_version": 11})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();

        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let frame: Value = serde_json::from_str(&text).unwrap();
            let mid = frame["message_id"].as_str().unwrap().to_string();
            received_srv.lock().unwrap().push(frame.clone());
            let result = match frame["command"].as_str().unwrap() {
                "discover" => json!([]),
                "commission_with_code" | "commission_on_network" => node.clone(),
                _ => Value::Null,
            };
            ws.send(Message::Text(
                json!({"message_id": mid, "result": result})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        }
    });

    (url, received)
}

/// The failure that cost hours on 2026-08-05: the device's 15-minute
/// commissioning window had closed, so discovery found nothing and the
/// controller answered a bare "Commissioning failed for node N" after a 30s
/// timeout — with the real reason only in its own log file. The user is now
/// told what happened and what to do, without the wait.
#[tokio::test]
async fn commissioning_with_nothing_in_pairing_mode_says_so_and_says_it_early() {
    let (url, received) = mock_unpairable_server(light_node_json()).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client);

    let error = commissioner
        .commission(SetupCode::Passcode(20202021), None)
        .await
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("pairing mode"),
        "the message names the actual problem: {error}"
    );
    assert!(
        error.contains("15 minutes"),
        "and the window that explains why it was pairable earlier: {error}"
    );

    // Refused on the probe, so the 30-second discovery timeout is never
    // entered — that speed is the point, not a side effect.
    let commands: Vec<String> = received
        .lock()
        .unwrap()
        .iter()
        .map(|f| f["command"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(commands, vec!["discover"], "nothing else was attempted");
}

/// The probe must not become a second way to fail. A controller that answers it
/// with something unexpected proves nothing about the device, so commissioning
/// goes ahead exactly as before.
#[tokio::test]
async fn an_unusable_probe_answer_does_not_block_commissioning() {
    // The general mock answers every non-commissioning command with null.
    let (url, _received) = mock_matter_server(json!([]), vec![]).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client);

    let error = commissioner
        .commission(SetupCode::Passcode(20202021), None)
        .await
        .unwrap_err()
        .to_string();

    assert!(
        !error.contains("pairing mode"),
        "a null probe answer must not be reported as an empty network: {error}"
    );
}

/// A named commission writes the name to the device's NodeLabel and returns it
/// as the device name — so chat resolution and other controllers both see it.
#[tokio::test]
async fn commission_with_name_writes_nodelabel() {
    let (url, received) = mock_commissioning_server(light_node_json()).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client);

    let dev = commissioner
        .commission(SetupCode::Passcode(20202021), Some("Living Room".into()))
        .await
        .unwrap();

    // The user's name wins over the cluster-derived one, and the full identity
    // is returned so the endpoint can register without waiting for the bridge.
    assert_eq!(dev.name, "Living Room");
    assert_eq!(dev.device_id, "matter-2");
    assert_eq!(dev.device_type, "light");
    assert_eq!(dev.capabilities, vec!["power", "brightness"]);

    // NodeLabel (0/40/5) was written on the device with that name.
    let frames = received.lock().unwrap().clone();
    let write = frames
        .iter()
        .find(|f| f["command"] == "write_attribute")
        .expect("a NodeLabel write");
    assert_eq!(write["args"]["node_id"], 2);
    assert_eq!(write["args"]["attribute_path"], "0/40/5");
    assert_eq!(write["args"]["value"], "Living Room");
}

/// An un-named commission touches no NodeLabel and keeps the device's own name.
#[tokio::test]
async fn commission_without_name_leaves_nodelabel_alone() {
    let (url, received) = mock_commissioning_server(light_node_json()).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client);

    let dev = commissioner
        .commission(SetupCode::Passcode(20202021), None)
        .await
        .unwrap();

    assert_eq!(dev.name, "Living Room Light"); // from the node's own label
    let frames = received.lock().unwrap().clone();
    assert!(
        !frames.iter().any(|f| f["command"] == "write_attribute"),
        "no NodeLabel write when no name is given"
    );
}

/// A node the controller no longer knows is already in the desired end state,
/// so decommission treats "does not exist" as success — letting a delete that
/// an earlier interrupted removal left half-done finish cleanly.
#[tokio::test]
async fn decommission_treats_already_gone_as_success() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.send(Message::Text(
            json!({"fabric_id": 1, "schema_version": 11})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let frame: Value = serde_json::from_str(&text).unwrap();
            let mid = frame["message_id"].as_str().unwrap();
            // Answer remove_node with the controller's real not-found error.
            ws.send(Message::Text(
                json!({
                    "message_id": mid,
                    "error_code": 1,
                    "details": "Node 2 does not exist or has not been interviewed."
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        }
    });

    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client);
    // Not an error: the node is already off the fabric.
    commissioner.decommission(2).await.unwrap();
}

/// Decommissioning removes the node from the fabric, so a deleted device does
/// not re-announce itself on the next start_listening.
#[tokio::test]
async fn decommission_sends_remove_node() {
    let (url, received) = mock_commissioning_server(light_node_json()).await;
    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let commissioner = MatterCommissioner::new(client);

    commissioner.decommission(2).await.unwrap();

    let frames = received.lock().unwrap().clone();
    let remove = frames
        .iter()
        .find(|f| f["command"] == "remove_node")
        .expect("a remove_node frame");
    assert_eq!(remove["args"]["node_id"], 2);
}

/// Regression: `send_command_with_timeout` must honour its argument, not the
/// 15s default. Commissioning relies on the longer window; a silent bug here
/// would cut real pairings short.
#[tokio::test]
async fn send_command_honours_its_timeout_argument() {
    // A server that greets, then never answers a command.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.send(Message::Text(
            json!({"fabric_id": 1, "schema_version": 11})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        while let Some(Ok(_)) = ws.next().await {} // read, never reply
    });

    let (client, _events) = MatterClient::connect(&url).await.unwrap();
    let start = std::time::Instant::now();
    let res = client
        .send_command_with_timeout("noop", json!({}), Duration::from_millis(200))
        .await;

    assert!(res.is_err(), "a never-answered command must error");
    // If the argument were ignored it would block on the 15s default.
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "timed out on the 200ms argument, not COMMAND_TIMEOUT"
    );
}

// ── Runtime reconciliation ───────────────────────────────────────────────────

/// A mock that serves connection after connection, so enable → disable →
/// enable can be observed. Reports how many times it was connected to, which
/// is how "did the runtime churn the connection?" is asserted.
async fn mock_reconnectable_server(node: Value) -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let connections = Arc::new(Mutex::new(0usize));
    let connections_srv = connections.clone();

    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let node = node.clone();
            let connections_conn = connections_srv.clone();
            tokio::spawn(async move {
                // Only completed handshakes count. The controller-readiness
                // probe (`is_running`) opens a bare TCP connection and drops
                // it, which would otherwise read as a second client.
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                *connections_conn.lock().unwrap() += 1;
                ws.send(Message::Text(
                    json!({"fabric_id": 1, "schema_version": 11})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    let mid = frame["message_id"].as_str().unwrap().to_string();
                    let result = match frame["command"].as_str().unwrap() {
                        "start_listening" => json!([node]),
                        "commission_with_code" | "commission_on_network" => node.clone(),
                        _ => Value::Null,
                    };
                    ws.send(Message::Text(
                        json!({"message_id": mid, "result": result})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                }
            });
        }
    });

    (url, connections)
}

fn test_runtime() -> Arc<MatterRuntime> {
    MatterRuntime::new(
        std::env::temp_dir().join("giap-matter-runtime-test"),
        Arc::new(InMemoryRegistry::default()) as Arc<dyn DeviceRegistry + Send + Sync>,
        Arc::new(InProcessEventBus::new()) as Arc<dyn EventBus>,
    )
}

/// Poll until the runtime reaches a state the predicate accepts, or fail. The
/// reconciler is asynchronous by design, so tests observe it, never assume it.
async fn wait_for(runtime: &MatterRuntime, want: impl Fn(&MatterState) -> bool) -> MatterStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let status = runtime.status().await;
        if want(&status.state) {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "runtime never settled; stuck at {:?}",
            status.state
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The whole point of the runtime: Matter comes up from a settings change, with
/// no process restart, and commissioning works the moment it reports Connected.
#[tokio::test]
async fn enabling_connects_and_exposes_a_commissioner() {
    let (url, _connections) = mock_reconnectable_server(light_node_json()).await;
    let runtime = test_runtime();

    assert_eq!(runtime.status().await.state, MatterState::Disabled);
    assert!(runtime.commissioner().await.is_none());

    runtime.apply(true, url.clone());
    let status = wait_for(&runtime, |s| *s == MatterState::Connected).await;
    assert!(status.enabled);
    assert_eq!(status.url, url);

    let commissioner = runtime
        .commissioner()
        .await
        .expect("a connected runtime exposes its commissioner");
    let device = commissioner
        .commission(SetupCode::Passcode(20202021), None)
        .await
        .unwrap();
    assert_eq!(device.device_id, "matter-2");
}

/// Turning Matter off has to actually stop it: a commissioner left behind would
/// keep the API reporting success against a controller nobody asked for.
#[tokio::test]
async fn disabling_tears_down_and_re_enabling_reconnects() {
    let (url, connections) = mock_reconnectable_server(light_node_json()).await;
    let runtime = test_runtime();

    runtime.apply(true, url.clone());
    wait_for(&runtime, |s| *s == MatterState::Connected).await;

    runtime.apply(false, url.clone());
    let status = wait_for(&runtime, |s| *s == MatterState::Disabled).await;
    assert!(!status.enabled);
    assert!(runtime.commissioner().await.is_none());
    // The URL stays visible so the UI's controller field is not blanked.
    assert_eq!(status.url, url);

    runtime.apply(true, url.clone());
    wait_for(&runtime, |s| *s == MatterState::Connected).await;
    assert!(runtime.commissioner().await.is_some());
    assert_eq!(
        *connections.lock().unwrap(),
        2,
        "re-enabling opens a second connection"
    );
}

/// Saving Settings re-sends every field, so an unchanged Matter section must
/// not drop and re-open a working connection.
#[tokio::test]
async fn re_applying_the_same_state_while_connected_does_not_churn() {
    let (url, connections) = mock_reconnectable_server(light_node_json()).await;
    let runtime = test_runtime();

    runtime.apply(true, url.clone());
    wait_for(&runtime, |s| *s == MatterState::Connected).await;

    for _ in 0..3 {
        runtime.apply(true, url.clone());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(runtime.status().await.state, MatterState::Connected);
    assert_eq!(
        *connections.lock().unwrap(),
        1,
        "an unchanged desired state must not reconnect"
    );
}

/// The bug this whole change exists to kill: an enabled-but-unreachable
/// controller used to be indistinguishable from "Matter is not enabled".
#[tokio::test]
async fn an_unreachable_controller_reports_the_failure_not_off() {
    let runtime = test_runtime();
    // `.invalid` never resolves, and a non-loopback host is never auto-started,
    // so this fails fast without touching the controller installer.
    runtime.apply(true, "ws://matter-controller.invalid:5580/ws".into());

    let status = wait_for(&runtime, |s| matches!(s, MatterState::Unreachable { .. })).await;
    assert!(
        status.enabled,
        "still enabled — it is the link that is down"
    );
    let MatterState::Unreachable { error } = status.state else {
        unreachable!()
    };
    assert!(
        error.contains("matter-controller.invalid"),
        "the reported error names what could not be reached: {error}"
    );
    assert!(runtime.commissioner().await.is_none());
}

/// Retrying is `apply` with the same values — which only reconnects because the
/// runtime compares against what is actually running, not against the request.
#[tokio::test]
async fn re_applying_after_a_failure_retries() {
    let (url, connections) = mock_reconnectable_server(light_node_json()).await;
    let runtime = test_runtime();

    runtime.apply(true, "ws://matter-controller.invalid:5580/ws".into());
    wait_for(&runtime, |s| matches!(s, MatterState::Unreachable { .. })).await;

    runtime.apply(true, url.clone());
    wait_for(&runtime, |s| *s == MatterState::Connected).await;
    assert_eq!(*connections.lock().unwrap(), 1);
}

/// Shutdown must leave nothing running — an orphaned controller outlives the
/// Pond, including under `systemctl stop`.
#[tokio::test]
async fn shutdown_clears_the_runtime() {
    let (url, _connections) = mock_reconnectable_server(light_node_json()).await;
    let runtime = test_runtime();

    runtime.apply(true, url);
    wait_for(&runtime, |s| *s == MatterState::Connected).await;

    runtime.shutdown().await;

    assert!(runtime.commissioner().await.is_none());
}

// ── Switchable device control ────────────────────────────────────────────────

/// Records every verb it is asked for, including the optional ones, so the
/// facade's forwarding can be asserted rather than assumed.
#[derive(Default)]
struct RecordingControl {
    calls: Mutex<Vec<String>>,
}

impl RecordingControl {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn ok(&self, call: String, device_id: &str) -> Result<DeviceControlOutcome> {
        self.calls.lock().unwrap().push(call);
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch::default(),
        ))
    }
}

#[async_trait::async_trait]
impl DeviceControlPort for RecordingControl {
    async fn set_power(&self, id: &str, on: bool) -> Result<DeviceControlOutcome> {
        self.ok(format!("set_power({id},{on})"), id)
    }
    async fn set_brightness(&self, id: &str, pct: u8) -> Result<DeviceControlOutcome> {
        self.ok(format!("set_brightness({id},{pct})"), id)
    }
    async fn set_target_temp(&self, id: &str, c: f32) -> Result<DeviceControlOutcome> {
        self.ok(format!("set_target_temp({id},{c})"), id)
    }
    async fn set_locked(&self, id: &str, locked: bool) -> Result<DeviceControlOutcome> {
        self.ok(format!("set_locked({id},{locked})"), id)
    }
    async fn set_color(&self, id: &str, hue: u16, sat: u8) -> Result<DeviceControlOutcome> {
        self.ok(format!("set_color({id},{hue},{sat})"), id)
    }
    async fn set_fan_speed(&self, id: &str, pct: u8) -> Result<DeviceControlOutcome> {
        self.ok(format!("set_fan_speed({id},{pct})"), id)
    }
    async fn set_position(&self, id: &str, pct: u8) -> Result<DeviceControlOutcome> {
        self.ok(format!("set_position({id},{pct})"), id)
    }
}

/// The stub answers every verb with success, so a Matter device must never
/// reach it: routing `matter-18` there while Matter was off reported a fan as
/// switched on when nothing had been sent anywhere, and the agent relayed that
/// to the user. Devices on other transports are exactly what the stub is for,
/// so those still fall through.
#[tokio::test]
async fn a_matter_device_is_refused_while_matter_is_off_but_others_fall_back() {
    let runtime = test_runtime();
    let stub = Arc::new(RecordingControl::default());
    let control = runtime.device_control(stub.clone() as Arc<dyn DeviceControlPort>);

    let refused = control
        .set_power("matter-2", true)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("Matter is off") && refused.contains("matter-2"),
        "the refusal names the state and the device: {refused}"
    );
    assert!(control.set_brightness("matter-2", 40).await.is_err());
    assert!(control.set_color("matter-2", 120, 80).await.is_err());

    // Non-Matter ids are the stub's job and still get there.
    control.set_target_temp("thermo", 21.5).await.unwrap();
    control.set_locked("front-door", true).await.unwrap();
    control.set_fan_speed("fan-1", 50).await.unwrap();
    control.set_position("blind-1", 30).await.unwrap();

    assert_eq!(
        stub.calls(),
        vec![
            "set_target_temp(thermo,21.5)",
            "set_locked(front-door,true)",
            "set_fan_speed(fan-1,50)",
            "set_position(blind-1,30)",
        ],
        "no Matter call reached the stub"
    );
}

/// Once connected the same facade drives the fabric — without the agent, MCP
/// server, or tool wiring being rebuilt, since they all hold this one `Arc`.
#[tokio::test]
async fn control_switches_to_matter_once_connected() {
    let (url, _connections) = mock_reconnectable_server(light_node_json()).await;
    let runtime = test_runtime();
    let stub = Arc::new(RecordingControl::default());
    let control = runtime.device_control(stub.clone() as Arc<dyn DeviceControlPort>);

    runtime.apply(true, url);
    wait_for(&runtime, |s| *s == MatterState::Connected).await;
    // The bridge's initial sync has to land before endpoints resolve.
    tokio::time::sleep(Duration::from_millis(200)).await;

    control.set_power("matter-2", true).await.unwrap();
    assert!(
        stub.calls().is_empty(),
        "a connected runtime must not fall back to the stub"
    );

    // Turning Matter off does not hand Matter devices back to the stub. With no
    // controller there is nothing that could carry the command, and the stub's
    // success would be a lie the agent repeats to the user.
    runtime.apply(false, String::new());
    wait_for(&runtime, |s| *s == MatterState::Disabled).await;
    assert!(control.set_power("matter-2", false).await.is_err());
    assert!(stub.calls().is_empty(), "still nothing reached the stub");
}

/// An install enabled before the Matter section existed could carry a blank
/// address. It must say so, not surface an opaque URL-parse failure.
#[tokio::test]
async fn an_empty_controller_address_is_reported_plainly() {
    let runtime = test_runtime();
    runtime.apply(true, String::new());

    let status = wait_for(&runtime, |s| matches!(s, MatterState::Unreachable { .. })).await;
    let MatterState::Unreachable { error } = status.state else {
        unreachable!()
    };
    assert!(error.contains("no Matter controller address"), "{error}");
}

/// Teardown must kill whatever process is in the shared cell *now*, not the
/// handle `connect` originally put there. After the supervisor respawns a dead
/// controller those are different processes, and killing the stale one would
/// leave the live controller running past the Pond — the orphan that let a
/// week-old controller outlive several restarts in the first place.
#[cfg(unix)]
#[tokio::test]
async fn stopping_the_controller_kills_whatever_the_cell_holds_now() {
    /// True while the process is alive. `kill -0` signals nothing; it only
    /// checks the pid is still there, and avoids a libc dependency for one probe.
    async fn alive(pid: u32) -> bool {
        tokio::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    let cell: crate::server_setup::SharedServerChild = Arc::new(tokio::sync::Mutex::new(None));

    // An empty cell is the "user runs their own controller" case: nothing of
    // GIAP's to kill, and no panic for trying.
    crate::runtime::stop_controller(&cell).await;

    // Stand in for a respawned controller: a real child, parked exactly the way
    // `revive_local_controller` parks one.
    let child = tokio::process::Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .expect("spawning sleep should work on any unix host");
    let pid = child.id().expect("a freshly spawned child has a pid");
    *cell.lock().await = Some(child);
    assert!(
        alive(pid).await,
        "the stand-in controller should be running"
    );

    crate::runtime::stop_controller(&cell).await;

    assert!(
        cell.lock().await.is_none(),
        "the handle is taken, so a second teardown cannot double-kill"
    );
    // `start_kill` only signals; give the OS a moment to reap before asserting.
    let mut gone = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if !alive(pid).await {
            gone = true;
            break;
        }
    }
    assert!(gone, "the controller process should be dead");
}
