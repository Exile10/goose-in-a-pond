//! SQLite-backed implementation of `RoomRegistry` over the `rooms` table.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::user_data::ports::room_registry::{Room, RoomRegistry};
use sqlx::{Pool, Sqlite};
use uuid::Uuid;

pub struct SqliteRoomRegistry {
    pool: Pool<Sqlite>,
}

impl SqliteRoomRegistry {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct RoomRow {
    id: String,
    name: String,
    created_at: String,
}

impl From<RoomRow> for Room {
    fn from(row: RoomRow) -> Self {
        Room {
            id: row.id,
            name: row.name,
            created_at: row.created_at,
        }
    }
}

#[async_trait]
impl RoomRegistry for SqliteRoomRegistry {
    async fn create(&self, name: &str) -> Result<Room> {
        let id = Uuid::new_v4().to_string();
        let row: RoomRow = sqlx::query_as(
            "INSERT INTO rooms (id, name) VALUES (?, ?) RETURNING id, name, created_at",
        )
        .bind(&id)
        .bind(name)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    async fn list_rooms(&self) -> Result<Vec<Room>> {
        let rows: Vec<RoomRow> =
            sqlx::query_as("SELECT id, name, created_at FROM rooms ORDER BY created_at ASC")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(Room::from).collect())
    }

    async fn get_room(&self, room_id: &str) -> Result<Option<Room>> {
        let row: Option<RoomRow> =
            sqlx::query_as("SELECT id, name, created_at FROM rooms WHERE id = ?")
                .bind(room_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(Room::from))
    }

    async fn rename(&self, room_id: &str, name: &str) -> Result<()> {
        sqlx::query("UPDATE rooms SET name = ? WHERE id = ?")
            .bind(name)
            .bind(room_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete(&self, room_id: &str) -> Result<()> {
        let (occupants,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM devices WHERE room_id = ?")
            .bind(room_id)
            .fetch_one(&self.pool)
            .await?;
        if occupants > 0 {
            anyhow::bail!(
                "room '{room_id}' still has {occupants} device(s) in it; move or remove them first"
            );
        }
        sqlx::query("DELETE FROM rooms WHERE id = ?")
            .bind(room_id)
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

    async fn make_registry() -> (SqliteRoomRegistry, Database, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let registry = SqliteRoomRegistry::new(db.system.clone());
        (registry, db, tmp)
    }

    #[tokio::test]
    async fn create_and_list() {
        let (reg, _db, _tmp) = make_registry().await;
        let room = reg.create("Kitchen").await.unwrap();
        assert_eq!(room.name, "Kitchen");

        let rooms = reg.list_rooms().await.unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].id, room.id);
    }

    #[tokio::test]
    async fn get_room_returns_none_for_unknown() {
        let (reg, _db, _tmp) = make_registry().await;
        assert!(reg.get_room("nonexistent").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rename_updates_the_name() {
        let (reg, _db, _tmp) = make_registry().await;
        let room = reg.create("Kitchen").await.unwrap();
        reg.rename(&room.id, "Kitchen & Dining").await.unwrap();
        let fetched = reg.get_room(&room.id).await.unwrap().unwrap();
        assert_eq!(fetched.name, "Kitchen & Dining");
    }

    #[tokio::test]
    async fn delete_removes_an_empty_room() {
        let (reg, _db, _tmp) = make_registry().await;
        let room = reg.create("Kitchen").await.unwrap();
        reg.delete(&room.id).await.unwrap();
        assert!(reg.get_room(&room.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_refuses_a_room_with_devices_in_it() {
        let (reg, db, _tmp) = make_registry().await;
        let room = reg.create("Kitchen").await.unwrap();

        sqlx::query(
            "INSERT INTO devices (id, name, device_type, capabilities, room_id, created_at, \
             updated_at) VALUES ('d1', 'Lamp', 'light', '[]', ?, datetime('now'), datetime('now'))",
        )
        .bind(&room.id)
        .execute(&db.system)
        .await
        .unwrap();

        let result = reg.delete(&room.id).await;
        assert!(result.is_err());
        assert!(reg.get_room(&room.id).await.unwrap().is_some());
    }
}
