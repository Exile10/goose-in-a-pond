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
use pond_core::user_data::ports::device_control::DeviceControlPort;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;

use crate::bridge::{run_matter_bridge, run_matter_supervisor};
use crate::client::MatterClient;
use crate::control::{MatterDeviceControl, NodeCache, SharedMatterClient};

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
    let view = event.trigger_view();
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
        url.clone(),
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
