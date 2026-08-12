//! SensorStorage port — driven port for recording IoT sensor readings.

use crate::user_data::domain::sensor::{SensorAggregate, SensorReading};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[async_trait]
pub trait SensorStorage: Send + Sync {
    /// Record a new sensor reading.
    async fn record(&self, reading: SensorReading) -> Result<()>;

    /// Get the most recent reading for a specific device + sensor type.
    async fn get_latest(&self, device_id: &str, sensor_type: &str)
        -> Result<Option<SensorReading>>;

    /// Get the `limit` most recent readings for a device (all sensor types), newest first.
    async fn get_recent(&self, device_id: &str, limit: usize) -> Result<Vec<SensorReading>>;

    /// Get readings for a device + sensor type within an optional time range, newest first.
    async fn get_history(
        &self,
        device_id: &str,
        sensor_type: &str,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<Vec<SensorReading>>;

    /// Like [`SensorStorage::get_history`], but capped at `limit` rows (newest
    /// first), so a caller serving a request cannot pull a whole retention
    /// window into memory.
    ///
    /// The default post-truncates `get_history`; an adapter backed by a query
    /// engine should override it so the bound reaches the query.
    async fn get_history_limited(
        &self,
        device_id: &str,
        sensor_type: &str,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<SensorReading>> {
        let mut readings = self
            .get_history(device_id, sensor_type, since, until)
            .await?;
        readings.truncate(limit);
        Ok(readings)
    }

    /// Summary statistics over a window.
    ///
    /// The default folds `get_history` so in-memory stores need no extra code;
    /// a SQL-backed adapter should override it with a single aggregate query
    /// that never materialises the rows.
    async fn aggregate(
        &self,
        device_id: &str,
        sensor_type: &str,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<SensorAggregate> {
        let readings = self
            .get_history(device_id, sensor_type, since, until)
            .await?;
        let count = readings.len() as u64;
        if count == 0 {
            return Ok(SensorAggregate {
                count: 0,
                min: None,
                max: None,
                avg: None,
                unit: None,
            });
        }
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut sum = 0.0;
        for r in &readings {
            min = min.min(r.value);
            max = max.max(r.value);
            sum += r.value;
        }
        Ok(SensorAggregate {
            count,
            min: Some(min),
            max: Some(max),
            avg: Some(sum / count as f64),
            unit: Some(readings[0].unit.clone()),
        })
    }

    /// List all distinct (device_id, sensor_type) pairs in the store.
    async fn list_sensors(&self) -> Result<Vec<(String, String)>>;
}
