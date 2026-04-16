/// Background notification poller.
///
/// Polls pond-server every 30 s for notable events and fires OS notifications:
///   - Device online / offline state changes
///   - New unacknowledged camera events
///
/// pond-server's auth middleware validates token *format* only (any well-formed
/// Bearer string is accepted on loopback), so we use a fixed internal token.
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

const INTERNAL_BEARER: &str = "Bearer giap-desktop";
const POLL_INTERVAL_SECS: u64 = 30;

#[derive(Deserialize)]
struct DeviceList {
    devices: Vec<DeviceInfo>,
}

#[derive(Deserialize)]
struct DeviceInfo {
    id: String,
    name: String,
    is_online: bool,
}

#[derive(Deserialize)]
struct CameraEventList {
    events: Vec<CameraEvent>,
}

#[derive(Deserialize)]
struct CameraEvent {
    id: i64,
    camera_id: String,
    event_type: String,
    acknowledged: bool,
}

/// Spawn and run the notification poller forever (call from `tauri::async_runtime::spawn`).
pub async fn run(app: AppHandle, base_url: String) {
    let client = Client::new();

    let mut device_states: HashMap<String, bool> = HashMap::new();
    let mut last_camera_id: i64 = 0;
    let mut initialized = false;

    loop {
        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;

        // ── Devices ──────────────────────────────────────────────────────────
        match fetch_devices(&client, &base_url).await {
            Ok(list) => {
                for d in &list.devices {
                    if initialized {
                        match device_states.get(&d.id).copied() {
                            Some(true) if !d.is_online => {
                                notify(&app, "Device Offline", &format!("{} went offline", d.name));
                            }
                            Some(false) if d.is_online => {
                                notify(&app, "Device Online", &format!("{} is back online", d.name));
                            }
                            _ => {}
                        }
                    }
                    device_states.insert(d.id.clone(), d.is_online);
                }
            }
            Err(e) => tracing::debug!("Notification poller — devices fetch error: {e}"),
        }

        // ── Camera events ─────────────────────────────────────────────────────
        match fetch_camera_events(&client, &base_url).await {
            Ok(cam) => {
                if !initialized {
                    // Anchor to the latest event so we don't replay history on startup.
                    if let Some(first) = cam.events.first() {
                        last_camera_id = first.id;
                    }
                } else {
                    let new_events: Vec<_> = cam
                        .events
                        .iter()
                        .filter(|e| e.id > last_camera_id && !e.acknowledged)
                        .collect();

                    for event in new_events.iter().take(3) {
                        let body = format!(
                            "{}: {}",
                            event.camera_id,
                            event.event_type.replace('_', " ").to_lowercase()
                        );
                        notify(&app, "Camera Alert", &body);
                        if event.id > last_camera_id {
                            last_camera_id = event.id;
                        }
                    }
                }
            }
            Err(e) => tracing::debug!("Notification poller — camera fetch error: {e}"),
        }

        initialized = true;
    }
}

async fn fetch_devices(client: &Client, base_url: &str) -> Result<DeviceList, String> {
    client
        .get(format!("{}/api/v1/devices", base_url))
        .header("Authorization", INTERNAL_BEARER)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<DeviceList>()
        .await
        .map_err(|e| e.to_string())
}

async fn fetch_camera_events(client: &Client, base_url: &str) -> Result<CameraEventList, String> {
    client
        .get(format!("{}/api/v1/camera/events?limit=10", base_url))
        .header("Authorization", INTERNAL_BEARER)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<CameraEventList>()
        .await
        .map_err(|e| e.to_string())
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    let result = app.notification().builder().title(title).body(body).show();
    if let Err(e) = result {
        tracing::debug!("Notification failed: {e}");
    }
}
