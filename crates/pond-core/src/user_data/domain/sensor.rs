//! Sensor and camera domain types for the data pipeline.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single reading from a physical sensor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorReading {
    pub device_id: String,
    /// Sensor category, e.g. "temperature", "humidity", "motion", "co2"
    pub sensor_type: String,
    pub value: f64,
    /// Unit string, e.g. "C", "F", "%", "ppm"
    pub unit: String,
    pub recorded_at: DateTime<Utc>,
}

/// An event detected by a camera or vision system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraEvent {
    /// Set by the DB on insert; `None` before persisting
    pub id: Option<i64>,
    pub camera_id: String,
    /// Event category, e.g. "motion", "person", "vehicle", "package"
    pub event_type: String,
    pub confidence: Option<f64>,
    pub snapshot_path: Option<String>,
    /// Arbitrary JSON string for extra metadata
    pub metadata: Option<String>,
    pub acknowledged: bool,
    pub created_at: DateTime<Utc>,
}
