//! CameraStorage port — driven port for recording camera / vision events.

use crate::domain::sensor::CameraEvent;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait CameraStorage: Send + Sync {
    /// Record a new camera event. Returns the database-assigned event ID.
    async fn record_event(&self, event: CameraEvent) -> Result<i64>;

    /// List the `limit` most recent events for a camera, newest first.
    async fn list_events(&self, camera_id: &str, limit: usize) -> Result<Vec<CameraEvent>>;

    /// Mark an event as acknowledged (suppresses re-alert).
    async fn acknowledge(&self, event_id: i64) -> Result<()>;
}
