//! SQLite-backed implementation of `DeviceRegistry`.
//!
//! Uses the `devices` table in `pond_system.db`.
//! Columns `device_type`, `ip_address`, `capabilities`, `last_seen`, `is_online`
//! are added by migration `0004_devices_enhanced.sql`.
//!
//! Timestamps are written as RFC 3339 and read leniently, because older rows hold
//! naive UTC (`2026-08-17 21:40:55`) from when this file wrote that. Naive is
//! unambiguous only to a reader who already knows it means UTC, and the desktop's
//! `new Date(...)` does not: it reads a zoneless string as local time, which showed
//! a device heartbeated seconds ago as hours stale.
//!
//! `is_online` is derived at read-time by comparing `last_seen` to `now - 5 min`
//! (heartbeat threshold), and nothing else. The stored `is_online` column is
//! written but never read back — `row_to_device` does not even select it — so a
//! device is online exactly as long as something keeps saying so.
//!
//! That makes the freshness the whole contract, and it is a contract every source
//! of devices has to keep: a subsystem that only touches `last_seen` when something
//! happens will show its devices going offline while they sit there working. The
//! Matter bridge did precisely that until it grew a periodic tick.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use pond_core::user_data::ports::device_registry::{
    Device, DeviceRegistry, RegisterDeviceRequest, UpdateDeviceRequest,
};
use serde_json;
use sqlx::{Pool, Sqlite};
use uuid::Uuid;

const ONLINE_THRESHOLD_SECS: i64 = 300; // 5 minutes

pub struct SqliteDeviceRegistry {
    pool: Pool<Sqlite>,
}

impl SqliteDeviceRegistry {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

// ── Row helper ────────────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct DeviceRow {
    id: String,
    name: String,
    hostname: Option<String>,
    device_type: String,
    ip_address: Option<String>,
    capabilities: String,
    created_at: String,
    last_seen: Option<String>,
    room: Option<String>,
}

/// The instant a stored timestamp names, whichever way it was written.
///
/// This table holds two formats. `register` and `heartbeat` write naive UTC
/// (`2026-08-17 21:40:55`); other rows carry RFC 3339 with an offset
/// (`2026-07-25T01:09:58+00:00`). Reading only the first is what made a desktop
/// row permanently offline: its timestamp failed to parse, and an unparseable
/// `last_seen` falls back to "not online" no matter how recently it was touched.
fn parse_stored(value: &str) -> Option<chrono::DateTime<Utc>> {
    if let Ok(fixed) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(fixed.with_timezone(&Utc));
    }
    // Naive: written by this file, and UTC by construction.
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc())
}

/// A stored timestamp as an instant nothing can misread.
///
/// Naive UTC is unambiguous only to a reader who already knows it is UTC. The
/// desktop reads these with `new Date(...)`, which takes a string with no zone as
/// LOCAL time -- so a washer heartbeated seconds ago displayed as "3h ago" to a
/// reader in UTC+3, next to the "online" badge that the same timestamp had just
/// produced. Emitting an offset removes the guess rather than asking every consumer
/// to make it correctly.
fn as_instant(value: &str) -> String {
    parse_stored(value).map_or_else(|| value.to_string(), |t| t.to_rfc3339())
}

fn row_to_device(row: DeviceRow) -> Device {
    let capabilities: Vec<String> = serde_json::from_str(&row.capabilities).unwrap_or_default();

    // Online means something said so recently. An unparseable timestamp is not a
    // claim, so it reads as offline -- but it must fail to parse because it is
    // absent or corrupt, not merely because it was written in the other format.
    let is_online = row
        .last_seen
        .as_deref()
        .and_then(parse_stored)
        .map(|last| Utc::now() - last < Duration::seconds(ONLINE_THRESHOLD_SECS))
        .unwrap_or(false);

    Device {
        id: row.id,
        name: row.name,
        device_type: row.device_type,
        hostname: row.hostname,
        ip_address: row.ip_address,
        capabilities,
        registered_at: as_instant(&row.created_at),
        last_seen: row.last_seen.as_deref().map(as_instant),
        is_online,
        room: row.room,
    }
}

#[async_trait]
impl DeviceRegistry for SqliteDeviceRegistry {
    async fn register(&self, request: RegisterDeviceRequest) -> Result<Device> {
        let id = request
            .id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let caps_json = serde_json::to_string(&request.capabilities)?;
        let now_str = Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO devices (id, name, hostname, device_type, ip_address, capabilities, \
             is_online, created_at, updated_at, last_seen, room) \
             VALUES (?, ?, ?, ?, NULL, ?, 1, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&request.name)
        .bind(&request.hostname)
        .bind(&request.device_type)
        .bind(&caps_json)
        .bind(&now_str)
        .bind(&now_str)
        .bind(&now_str)
        .bind(&request.room)
        .execute(&self.pool)
        .await?;

        Ok(Device {
            id,
            name: request.name,
            device_type: request.device_type,
            hostname: request.hostname,
            ip_address: None,
            capabilities: request.capabilities,
            registered_at: now_str.clone(),
            last_seen: Some(now_str),
            is_online: true,
            room: request.room,
        })
    }

    async fn list_devices(&self) -> Result<Vec<Device>> {
        let rows: Vec<DeviceRow> = sqlx::query_as(
            "SELECT id, name, hostname, device_type, ip_address, capabilities, created_at, last_seen, room \
             FROM devices ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_device).collect())
    }

    async fn get_device(&self, device_id: &str) -> Result<Option<Device>> {
        let row: Option<DeviceRow> = sqlx::query_as(
            "SELECT id, name, hostname, device_type, ip_address, capabilities, created_at, last_seen, room \
             FROM devices WHERE id = ?",
        )
        .bind(device_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_device))
    }

    async fn unregister(&self, device_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM devices WHERE id = ?")
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn rename(&self, device_id: &str, name: &str) -> Result<()> {
        let now_str = Utc::now().to_rfc3339();
        sqlx::query("UPDATE devices SET name = ?, updated_at = ? WHERE id = ?")
            .bind(name)
            .bind(&now_str)
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn heartbeat(&self, device_id: &str) -> Result<()> {
        let now_str = Utc::now().to_rfc3339();
        sqlx::query("UPDATE devices SET last_seen = ?, is_online = 1, updated_at = ? WHERE id = ?")
            .bind(&now_str)
            .bind(&now_str)
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn set_offline(&self, device_id: &str) -> Result<()> {
        // `is_online` is derived at read-time from `last_seen` (see module docs),
        // so "go offline" means backdating `last_seen` past the threshold rather
        // than flipping a stored flag — this mirrors `heartbeat`, which marks
        // online the same way (fresh `last_seen`).
        let stale = (Utc::now() - Duration::seconds(ONLINE_THRESHOLD_SECS + 60)).to_rfc3339();
        sqlx::query("UPDATE devices SET last_seen = ?, is_online = 0, updated_at = ? WHERE id = ?")
            .bind(&stale)
            .bind(&stale)
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn update(&self, device_id: &str, request: UpdateDeviceRequest) -> Result<Device> {
        let now_str = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE devices SET name = ?, hostname = ?, room = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&request.name)
        .bind(&request.hostname)
        .bind(&request.room)
        .bind(&now_str)
        .bind(device_id)
        .execute(&self.pool)
        .await?;
        self.get_device(device_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("device '{device_id}' not found after update"))
    }

    async fn set_discovered_profile(
        &self,
        device_id: &str,
        device_type: &str,
        capabilities: &[String],
    ) -> Result<()> {
        // Deliberately does NOT touch name, hostname or room: those belong to
        // whoever configured the device, and this runs on every bridge sync.
        let caps_json = serde_json::to_string(capabilities)?;
        let now_str = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE devices SET device_type = ?, capabilities = ?, updated_at = ? WHERE id = ?",
        )
        .bind(device_type)
        .bind(&caps_json)
        .bind(&now_str)
        .bind(device_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    async fn make_registry() -> (SqliteDeviceRegistry, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (SqliteDeviceRegistry::new(db.system), tmp)
    }

    fn req(name: &str) -> RegisterDeviceRequest {
        RegisterDeviceRequest {
            id: None,
            name: name.to_string(),
            device_type: "gotg".to_string(),
            hostname: Some("phone.local".to_string()),
            capabilities: vec!["chat".to_string(), "tts".to_string()],
            room: None,
        }
    }

    #[tokio::test]
    async fn register_and_list() {
        let (reg, _tmp) = make_registry().await;
        let dev = reg.register(req("My Phone")).await.unwrap();
        assert_eq!(dev.name, "My Phone");
        assert!(dev.is_online); // just registered

        let devices = reg.list_devices().await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].capabilities, vec!["chat", "tts"]);
    }

    #[tokio::test]
    async fn get_device_returns_none_for_unknown() {
        let (reg, _tmp) = make_registry().await;
        let result = reg.get_device("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn unregister_removes_device() {
        let (reg, _tmp) = make_registry().await;
        let dev = reg.register(req("Phone")).await.unwrap();
        reg.unregister(&dev.id).await.unwrap();
        assert!(reg.get_device(&dev.id).await.unwrap().is_none());
    }

    /// Both bugs on the Devices screen came from this one place, in opposite
    /// directions: a Matter row read as online but displayed hours stale, and a
    /// desktop row that could never read online at all.
    #[test]
    fn a_timestamp_is_understood_whichever_way_it_was_written() {
        // What `register` and `heartbeat` used to write. UTC, but it does not say so.
        let naive = parse_stored("2026-08-17 21:40:55").expect("naive UTC parses");
        assert_eq!(naive.to_rfc3339(), "2026-08-17T21:40:55+00:00");

        // What other rows already held. Failing this is what pinned a device
        // offline no matter how recently anything touched it.
        let rfc = parse_stored("2026-07-25T01:09:58.212087+00:00").expect("rfc3339 parses");
        assert_eq!(rfc.to_rfc3339(), "2026-07-25T01:09:58.212087+00:00");

        // An offset is honoured rather than ignored.
        let offset = parse_stored("2026-08-17T21:40:55+03:00").expect("an offset parses");
        assert_eq!(offset.to_rfc3339(), "2026-08-17T18:40:55+00:00");

        assert!(parse_stored("").is_none());
        assert!(parse_stored("whenever").is_none());
    }

    /// The half the reader sees: a zoneless timestamp handed to `new Date(...)` is
    /// read as local time, so a device heartbeated seconds ago showed as "3h ago"
    /// beside the "online" badge the same value had just produced.
    #[test]
    fn what_leaves_this_layer_states_its_offset() {
        assert_eq!(
            as_instant("2026-08-17 21:40:55"),
            "2026-08-17T21:40:55+00:00"
        );
        // Already unambiguous: passed through unchanged.
        assert_eq!(
            as_instant("2026-07-25T01:09:58.212087+00:00"),
            "2026-07-25T01:09:58.212087+00:00"
        );
        // Unrecognisable: kept verbatim rather than replaced with a made-up time.
        assert_eq!(as_instant("whenever"), "whenever");
    }

    #[tokio::test]
    async fn heartbeat_updates_last_seen() {
        let (reg, _tmp) = make_registry().await;
        let dev = reg.register(req("Phone")).await.unwrap();
        reg.heartbeat(&dev.id).await.unwrap();
        let updated = reg.get_device(&dev.id).await.unwrap().unwrap();
        assert!(updated.last_seen.is_some());
        assert!(updated.is_online);
    }

    /// #195: bridge-supplied stable ids are honoured verbatim, so re-syncs
    /// address the same row instead of accumulating duplicates.
    #[tokio::test]
    async fn register_honours_caller_supplied_stable_id() {
        let (reg, _tmp) = make_registry().await;
        let with_id = RegisterDeviceRequest {
            id: Some("matter-2".to_string()),
            ..req("Virtual OnOff Light")
        };
        let dev = reg.register(with_id).await.unwrap();
        assert_eq!(dev.id, "matter-2");
        assert!(reg.get_device("matter-2").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn register_persists_room() {
        let (reg, _tmp) = make_registry().await;
        let with_room = RegisterDeviceRequest {
            id: None,
            room: Some("Living Room".to_string()),
            ..req("Living Room Lamp")
        };
        let dev = reg.register(with_room).await.unwrap();
        assert_eq!(dev.room.as_deref(), Some("Living Room"));

        let listed = reg.list_devices().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].room.as_deref(), Some("Living Room"));

        let fetched = reg.get_device(&dev.id).await.unwrap().unwrap();
        assert_eq!(fetched.room.as_deref(), Some("Living Room"));
    }

    #[tokio::test]
    async fn register_without_room_keeps_none() {
        let (reg, _tmp) = make_registry().await;
        let dev = reg.register(req("Roomless")).await.unwrap();
        assert!(dev.room.is_none());
        let listed = reg.list_devices().await.unwrap();
        assert!(listed[0].room.is_none());
    }

    /// "Turn off" in the Devices UI: a freshly-registered device (last_seen =
    /// now, so it would otherwise read online) must flip to offline.
    #[tokio::test]
    async fn set_offline_marks_a_freshly_registered_device_offline() {
        let (reg, _tmp) = make_registry().await;
        let dev = reg.register(req("Phone")).await.unwrap();
        assert!(dev.is_online);

        reg.set_offline(&dev.id).await.unwrap();
        let updated = reg.get_device(&dev.id).await.unwrap().unwrap();
        assert!(!updated.is_online);
    }

    /// "Turn on" after "Turn off": heartbeat must bring it back online.
    #[tokio::test]
    async fn heartbeat_reverses_a_previous_set_offline() {
        let (reg, _tmp) = make_registry().await;
        let dev = reg.register(req("Phone")).await.unwrap();
        reg.set_offline(&dev.id).await.unwrap();
        assert!(!reg.get_device(&dev.id).await.unwrap().unwrap().is_online);

        reg.heartbeat(&dev.id).await.unwrap();
        assert!(reg.get_device(&dev.id).await.unwrap().unwrap().is_online);
    }

    #[tokio::test]
    async fn update_changes_name_hostname_and_room() {
        let (reg, _tmp) = make_registry().await;
        let dev = reg.register(req("Old Name")).await.unwrap();

        let updated = reg
            .update(
                &dev.id,
                UpdateDeviceRequest {
                    name: "New Name".to_string(),
                    hostname: Some("new.local".to_string()),
                    room: Some("Kitchen".to_string()),
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.name, "New Name");
        assert_eq!(updated.hostname.as_deref(), Some("new.local"));
        assert_eq!(updated.room.as_deref(), Some("Kitchen"));

        let fetched = reg.get_device(&dev.id).await.unwrap().unwrap();
        assert_eq!(fetched.name, "New Name");
        assert_eq!(fetched.room.as_deref(), Some("Kitchen"));
    }

    #[tokio::test]
    async fn update_unknown_device_errors() {
        let (reg, _tmp) = make_registry().await;
        let result = reg
            .update(
                "nonexistent",
                UpdateDeviceRequest {
                    name: "Whatever".to_string(),
                    hostname: None,
                    room: None,
                },
            )
            .await;
        assert!(result.is_err());
    }
}
