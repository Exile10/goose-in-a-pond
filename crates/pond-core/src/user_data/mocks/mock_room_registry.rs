//! No-op mock implementation of `RoomRegistry` for use in tests and convenience factories.

use crate::user_data::ports::room_registry::{Room, RoomRegistry};
use anyhow::Result;
use async_trait::async_trait;

/// Always returns an empty list; for tests that don't exercise room management.
pub struct MockRoomRegistry;

#[async_trait]
impl RoomRegistry for MockRoomRegistry {
    async fn create(&self, _: &str) -> Result<Room> {
        Err(anyhow::anyhow!("MockRoomRegistry: create not implemented"))
    }

    async fn list_rooms(&self) -> Result<Vec<Room>> {
        Ok(vec![])
    }

    async fn get_room(&self, _: &str) -> Result<Option<Room>> {
        Ok(None)
    }

    async fn rename(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
