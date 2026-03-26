//! In-memory mock implementations of `SensorStorage` and `CameraStorage`.

use crate::domain::sensor::{CameraEvent, SensorReading};
use crate::ports::camera_storage::CameraStorage;
use crate::ports::sensor_storage::SensorStorage;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct MockSensorStorage {
    readings: Arc<RwLock<Vec<SensorReading>>>,
}

impl MockSensorStorage {
    pub fn new() -> Self {
        Self { readings: Arc::new(RwLock::new(Vec::new())) }
    }
}

impl Default for MockSensorStorage {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl SensorStorage for MockSensorStorage {
    async fn record(&self, reading: SensorReading) -> Result<()> {
        self.readings.write().await.push(reading);
        Ok(())
    }

    async fn get_latest(&self, device_id: &str, sensor_type: &str) -> Result<Option<SensorReading>> {
        let readings = self.readings.read().await;
        Ok(readings
            .iter()
            .filter(|r| r.device_id == device_id && r.sensor_type == sensor_type)
            .max_by_key(|r| r.recorded_at)
            .cloned())
    }

    async fn get_recent(&self, device_id: &str, limit: usize) -> Result<Vec<SensorReading>> {
        let readings = self.readings.read().await;
        let mut results: Vec<SensorReading> = readings
            .iter()
            .filter(|r| r.device_id == device_id)
            .cloned()
            .collect();
        results.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        results.truncate(limit);
        Ok(results)
    }
}

pub struct MockCameraStorage {
    events: Arc<RwLock<Vec<CameraEvent>>>,
    next_id: Arc<RwLock<i64>>,
}

impl MockCameraStorage {
    pub fn new() -> Self {
        Self {
            events: Arc::new(RwLock::new(Vec::new())),
            next_id: Arc::new(RwLock::new(1)),
        }
    }
}

impl Default for MockCameraStorage {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl CameraStorage for MockCameraStorage {
    async fn record_event(&self, mut event: CameraEvent) -> Result<i64> {
        let mut id = self.next_id.write().await;
        event.id = Some(*id);
        event.created_at = Utc::now();
        self.events.write().await.push(event);
        let assigned = *id;
        *id += 1;
        Ok(assigned)
    }

    async fn list_events(&self, camera_id: &str, limit: usize) -> Result<Vec<CameraEvent>> {
        let events = self.events.read().await;
        let mut results: Vec<CameraEvent> = events
            .iter()
            .filter(|e| e.camera_id == camera_id)
            .cloned()
            .collect();
        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        results.truncate(limit);
        Ok(results)
    }

    async fn acknowledge(&self, event_id: i64) -> Result<()> {
        let mut events = self.events.write().await;
        if let Some(e) = events.iter_mut().find(|e| e.id == Some(event_id)) {
            e.acknowledged = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(device_id: &str, sensor_type: &str, value: f64) -> SensorReading {
        SensorReading {
            device_id: device_id.to_string(),
            sensor_type: sensor_type.to_string(),
            value,
            unit: "C".to_string(),
            recorded_at: Utc::now(),
        }
    }

    fn camera_event(camera_id: &str) -> CameraEvent {
        CameraEvent {
            id: None,
            camera_id: camera_id.to_string(),
            event_type: "motion".to_string(),
            confidence: Some(0.9),
            snapshot_path: None,
            metadata: None,
            acknowledged: false,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn sensor_record_and_get_latest() {
        let storage = MockSensorStorage::new();
        storage.record(reading("room1", "temperature", 21.0)).await.unwrap();
        storage.record(reading("room1", "temperature", 22.5)).await.unwrap();
        let latest = storage.get_latest("room1", "temperature").await.unwrap();
        assert!(latest.is_some());
    }

    #[tokio::test]
    async fn camera_record_and_acknowledge() {
        let storage = MockCameraStorage::new();
        let id = storage.record_event(camera_event("front")).await.unwrap();
        assert_eq!(id, 1);
        storage.acknowledge(id).await.unwrap();
        let events = storage.list_events("front", 10).await.unwrap();
        assert!(events[0].acknowledged);
    }
}
