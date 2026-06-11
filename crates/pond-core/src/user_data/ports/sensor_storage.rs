//! SensorStorage port — driven port for recording IoT sensor readings.

use crate::user_data::domain::sensor::SensorReading;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait SensorStorage: Send + Sync {
    /// Record a new sensor reading.
    async fn record(&self, reading: SensorReading) -> Result<()>;

    /// Get the most recent reading for a specific device + sensor type.
    async fn get_latest(&self, device_id: &str, sensor_type: &str)
        -> Result<Option<SensorReading>>;

    /// Get the `limit` most recent readings for a device (all sensor types), newest first.
    async fn get_recent(&self, device_id: &str, limit: usize) -> Result<Vec<SensorReading>>;
}
