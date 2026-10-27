//! Driven port: rooms the household has named, that devices are placed into.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A named room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub id: String,
    pub name: String,
    pub created_at: String,
}

/// Driven Port: room lifecycle management.
#[async_trait]
pub trait RoomRegistry: Send + Sync {
    /// Create a new room with the given name.
    async fn create(&self, name: &str) -> Result<Room>;

    /// List all rooms.
    async fn list_rooms(&self) -> Result<Vec<Room>>;

    /// Get a specific room by id.
    async fn get_room(&self, room_id: &str) -> Result<Option<Room>>;

    /// Rename a room.
    async fn rename(&self, room_id: &str, name: &str) -> Result<()>;

    /// Delete a room. Refuses while any device still points at it.
    async fn delete(&self, room_id: &str) -> Result<()>;
}
