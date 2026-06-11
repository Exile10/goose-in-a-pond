//! SQLite-backed implementations of `SensorStorage` and `CameraStorage`.
//!
//! IMPORTANT: Both use `db.logs.clone()` — sensor_readings and camera_events
//! are in `pond_logs.db`, NOT `pond_system.db`.

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use pond_core::user_data::domain::sensor::{CameraEvent, SensorReading};
use pond_core::user_data::ports::camera_storage::CameraStorage;
use pond_core::user_data::ports::sensor_storage::SensorStorage;
use sqlx::{Pool, Sqlite};

// ─────────────────────────────────────────────────────────────────────────────
// SensorStorage
// ─────────────────────────────────────────────────────────────────────────────

pub struct SqliteSensorStorage {
    pool: Pool<Sqlite>,
}

impl SqliteSensorStorage {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct SensorRow {
    device_id: String,
    sensor_type: String,
    value: f64,
    unit: String,
    created_at: String,
}

fn parse_dt(s: &str) -> chrono::DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or_else(|_| Utc::now())
}

fn sensor_row_to_reading(row: SensorRow) -> SensorReading {
    SensorReading {
        device_id: row.device_id,
        sensor_type: row.sensor_type,
        value: row.value,
        unit: row.unit,
        recorded_at: parse_dt(&row.created_at),
    }
}

#[async_trait]
impl SensorStorage for SqliteSensorStorage {
    async fn record(&self, reading: SensorReading) -> Result<()> {
        sqlx::query(
            "INSERT INTO sensor_readings (device_id, sensor_type, value, unit, created_at) \
             VALUES (?, ?, ?, ?, datetime('now'))",
        )
        .bind(&reading.device_id)
        .bind(&reading.sensor_type)
        .bind(reading.value)
        .bind(&reading.unit)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_latest(
        &self,
        device_id: &str,
        sensor_type: &str,
    ) -> Result<Option<SensorReading>> {
        let row: Option<SensorRow> = sqlx::query_as(
            "SELECT device_id, sensor_type, value, unit, created_at \
             FROM sensor_readings \
             WHERE device_id = ? AND sensor_type = ? \
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
        )
        .bind(device_id)
        .bind(sensor_type)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(sensor_row_to_reading))
    }

    async fn get_recent(&self, device_id: &str, limit: usize) -> Result<Vec<SensorReading>> {
        let rows: Vec<SensorRow> = sqlx::query_as(
            "SELECT device_id, sensor_type, value, unit, created_at \
             FROM sensor_readings \
             WHERE device_id = ? \
             ORDER BY created_at DESC, rowid DESC LIMIT ?",
        )
        .bind(device_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(sensor_row_to_reading).collect())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CameraStorage
// ─────────────────────────────────────────────────────────────────────────────

pub struct SqliteCameraStorage {
    pool: Pool<Sqlite>,
}

impl SqliteCameraStorage {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct CameraRow {
    id: i64,
    camera_id: String,
    event_type: String,
    confidence: Option<f64>,
    snapshot_path: Option<String>,
    metadata: Option<String>,
    acknowledged: i64,
    created_at: String,
}

fn camera_row_to_event(row: CameraRow) -> CameraEvent {
    CameraEvent {
        id: Some(row.id),
        camera_id: row.camera_id,
        event_type: row.event_type,
        confidence: row.confidence,
        snapshot_path: row.snapshot_path,
        metadata: row.metadata,
        acknowledged: row.acknowledged != 0,
        created_at: parse_dt(&row.created_at),
    }
}

#[async_trait]
impl CameraStorage for SqliteCameraStorage {
    async fn record_event(&self, event: CameraEvent) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO camera_events \
             (camera_id, event_type, confidence, snapshot_path, metadata, acknowledged, created_at) \
             VALUES (?, ?, ?, ?, ?, 0, datetime('now'))",
        )
        .bind(&event.camera_id)
        .bind(&event.event_type)
        .bind(event.confidence)
        .bind(&event.snapshot_path)
        .bind(&event.metadata)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    async fn list_events(&self, camera_id: &str, limit: usize) -> Result<Vec<CameraEvent>> {
        let rows: Vec<CameraRow> = sqlx::query_as(
            "SELECT id, camera_id, event_type, confidence, snapshot_path, metadata, \
             acknowledged, created_at \
             FROM camera_events WHERE camera_id = ? \
             ORDER BY created_at DESC, id DESC LIMIT ?",
        )
        .bind(camera_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(camera_row_to_event).collect())
    }

    async fn acknowledge(&self, event_id: i64) -> Result<()> {
        sqlx::query("UPDATE camera_events SET acknowledged = 1 WHERE id = ?")
            .bind(event_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use pond_core::user_data::domain::sensor::{CameraEvent, SensorReading};
    use tempfile::tempdir;

    async fn make_logs_pool() -> (Pool<Sqlite>, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (db.logs, tmp)
    }

    fn reading(device_id: &str, t: &str, v: f64) -> SensorReading {
        SensorReading {
            device_id: device_id.to_string(),
            sensor_type: t.to_string(),
            value: v,
            unit: "C".to_string(),
            recorded_at: Utc::now(),
        }
    }

    fn cam_event(camera_id: &str) -> CameraEvent {
        CameraEvent {
            id: None,
            camera_id: camera_id.to_string(),
            event_type: "motion".to_string(),
            confidence: Some(0.95),
            snapshot_path: None,
            metadata: None,
            acknowledged: false,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn sensor_record_and_get_latest() {
        let (pool, _tmp) = make_logs_pool().await;
        let storage = SqliteSensorStorage::new(pool);
        storage
            .record(reading("room1", "temperature", 21.0))
            .await
            .unwrap();
        storage
            .record(reading("room1", "temperature", 22.5))
            .await
            .unwrap();
        let latest = storage.get_latest("room1", "temperature").await.unwrap();
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().value, 22.5);
    }

    #[tokio::test]
    async fn sensor_get_recent_respects_limit() {
        let (pool, _tmp) = make_logs_pool().await;
        let storage = SqliteSensorStorage::new(pool);
        for i in 0..5 {
            storage
                .record(reading("dev1", "humidity", i as f64))
                .await
                .unwrap();
        }
        let recent = storage.get_recent("dev1", 3).await.unwrap();
        assert_eq!(recent.len(), 3);
    }

    #[tokio::test]
    async fn camera_record_and_acknowledge() {
        let (pool, _tmp) = make_logs_pool().await;
        let storage = SqliteCameraStorage::new(pool);
        let id = storage.record_event(cam_event("front")).await.unwrap();
        assert_eq!(id, 1);
        storage.acknowledge(id).await.unwrap();
        let events = storage.list_events("front", 10).await.unwrap();
        assert!(events[0].acknowledged);
    }

    #[tokio::test]
    async fn camera_list_respects_camera_id_filter() {
        let (pool, _tmp) = make_logs_pool().await;
        let storage = SqliteCameraStorage::new(pool);
        storage.record_event(cam_event("front")).await.unwrap();
        storage.record_event(cam_event("back")).await.unwrap();
        let front_events = storage.list_events("front", 10).await.unwrap();
        assert_eq!(front_events.len(), 1);
    }
}
