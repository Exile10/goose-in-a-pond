//! SQLite-backed implementation of `DeviceRegistry`.
//!
//! Uses the `devices` table in `pond_system.db`.
//! Columns `device_type`, `ip_address`, `capabilities`, `last_seen`, `is_online`
//! are added by migration `0004_devices_enhanced.sql`.
//!
//! `is_online` is derived at read-time by comparing `last_seen` to `now - 5 min`
//! (heartbeat threshold). The stored `is_online` column is used as a fallback
//! for devices that have never sent a heartbeat.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Duration, Utc};
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
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

fn row_to_device(row: DeviceRow) -> Device {
    let capabilities: Vec<String> = serde_json::from_str(&row.capabilities).unwrap_or_default();

    // Compute is_online by comparing last_seen to now - threshold
    let is_online = row
        .last_seen
        .as_deref()
        .and_then(|s| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|ndt| ndt.and_utc())
        })
        .map(|last| Utc::now() - last < Duration::seconds(ONLINE_THRESHOLD_SECS))
        .unwrap_or(false);

    Device {
        id: row.id,
        name: row.name,
        device_type: row.device_type,
        hostname: row.hostname,
        ip_address: row.ip_address,
        capabilities,
        registered_at: row.created_at,
        last_seen: row.last_seen,
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
        let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

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

    async fn heartbeat(&self, device_id: &str) -> Result<()> {
        let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        sqlx::query("UPDATE devices SET last_seen = ?, is_online = 1, updated_at = ? WHERE id = ?")
            .bind(&now_str)
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
}
