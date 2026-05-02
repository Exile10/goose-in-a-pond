/// Background notification poller.
///
/// Polls pond-server every 30 s for notable events and fires OS notifications:
///   - Device online / offline state changes
///   - New unacknowledged camera events
///   - Schedule task completions (new runs since last poll)
///
/// pond-server's auth middleware validates token *format* only (any well-formed
/// Bearer string is accepted on loopback), so we use a fixed internal token.
use reqwest::Client;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
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

#[derive(Deserialize)]
struct ScheduleInfo {
    id: String,
    label: String,
    #[serde(default)]
    last_run: Option<String>,
}

#[derive(Deserialize)]
struct ScheduleRunInfo {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Spawn and run the notification poller forever (call from `tauri::async_runtime::spawn`).
pub async fn run(app: AppHandle, base_url: String) {
    let client = Client::new();

    let mut device_states: HashMap<String, bool> = HashMap::new();
    let mut last_camera_id: i64 = 0;
    let mut seen_run_ids: HashSet<String> = HashSet::new();
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

        // ── Schedule results ─────────────────────────────────────────────────
        match fetch_schedules(&client, &base_url).await {
            Ok(schedules) => {
                for sched in &schedules {
                    // Only check schedules that have a last_run (i.e. have executed at least once).
                    if sched.last_run.is_none() {
                        continue;
                    }
                    match fetch_schedule_runs(&client, &base_url, &sched.id).await {
                        Ok(runs) => {
                            if !initialized {
                                // Seed: record all existing run IDs so we don't replay on startup.
                                for run in &runs {
                                    seen_run_ids.insert(run.id.clone());
                                }
                            } else {
                                // Notify on NEW completed/failed runs only.
                                for run in runs.iter().take(3) {
                                    let is_finished = run.status.as_deref()
                                        .map(|s| s == "completed" || s == "failed")
                                        .unwrap_or(false);
                                    if is_finished && !seen_run_ids.contains(&run.id) {
                                        let title = format!("Schedule: {}", sched.label);
                                        let body = match (&run.status, &run.result, &run.error) {
                                            (_, _, Some(err)) => truncate_preview(err, 120),
                                            (_, Some(result), _) => truncate_preview(result, 120),
                                            (Some(status), _, _) => status.clone(),
                                            _ => "Run finished".to_string(),
                                        };
                                        notify(&app, &title, &body);
                                    }
                                    seen_run_ids.insert(run.id.clone());
                                }
                            }
                        }
                        Err(e) => tracing::debug!(
                            "Notification poller — schedule runs fetch error for {}: {e}",
                            sched.id
                        ),
                    }
                }
            }
            Err(e) => tracing::debug!("Notification poller — schedules fetch error: {e}"),
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

async fn fetch_schedules(client: &Client, base_url: &str) -> Result<Vec<ScheduleInfo>, String> {
    client
        .get(format!("{}/api/v1/schedules", base_url))
        .header("Authorization", INTERNAL_BEARER)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<ScheduleInfo>>()
        .await
        .map_err(|e| e.to_string())
}

async fn fetch_schedule_runs(
    client: &Client,
    base_url: &str,
    schedule_id: &str,
) -> Result<Vec<ScheduleRunInfo>, String> {
    client
        .get(format!(
            "{}/api/v1/schedules/{}/runs?limit=5",
            base_url, schedule_id
        ))
        .header("Authorization", INTERNAL_BEARER)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<Vec<ScheduleRunInfo>>()
        .await
        .map_err(|e| e.to_string())
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let end = s.char_indices()
            .nth(max_len.saturating_sub(3))
            .map(|(i, _)| i)
            .unwrap_or(max_len.saturating_sub(3));
        format!("{}...", &s[..end])
    }
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    let result = app.notification().builder().title(title).body(body).show();
    if let Err(e) = result {
        tracing::debug!("Notification failed: {e}");
    }
}
