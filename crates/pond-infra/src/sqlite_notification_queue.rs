//! SQLite-backed offline notification queue (#99).
//!
//! Stores targeted notifications in the `notifications` table (migration 0027)
//! until the target device's stream delivers them.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification::Notification;
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use sqlx::{Pool, Row, Sqlite};

pub struct SqliteNotificationQueue {
    pool: Pool<Sqlite>,
}

impl SqliteNotificationQueue {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn row_to_notification(row: &sqlx::sqlite::SqliteRow) -> Notification {
    let data: Option<String> = row.get("data");
    Notification {
        id: row.get("id"),
        target: row.get("device_id"),
        category: row.get("category"),
        title: row.get("title"),
        body: row.get("body"),
        timestamp: row.get("timestamp"),
        data: data.and_then(|s| serde_json::from_str(&s).ok()),
    }
}

#[async_trait]
impl NotificationQueueRepository for SqliteNotificationQueue {
    async fn enqueue(&self, notification: Notification) -> Result<()> {
        let data = notification.data.as_ref().map(|v| v.to_string());
        sqlx::query(
            "INSERT OR REPLACE INTO notifications \
             (id, device_id, category, title, body, timestamp, data) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(notification.id)
        .bind(notification.target)
        .bind(notification.category)
        .bind(notification.title)
        .bind(notification.body)
        .bind(notification.timestamp)
        .bind(data)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_undelivered(&self, device_id: &str) -> Result<Vec<Notification>> {
        let rows = sqlx::query(
            "SELECT id, device_id, category, title, body, timestamp, data \
             FROM notifications \
             WHERE device_id = ? AND delivered_at IS NULL \
             ORDER BY created_at ASC, rowid ASC",
        )
        .bind(device_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_notification).collect())
    }

    async fn mark_delivered(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        // Build a parameterized `IN (?, ?, ...)` — never interpolate ids.
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "UPDATE notifications SET delivered_at = datetime('now') \
             WHERE id IN ({placeholders}) AND delivered_at IS NULL"
        );
        let mut q = sqlx::query(&sql);
        for id in ids {
            q = q.bind(id);
        }
        q.execute(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    async fn fresh() -> SqliteNotificationQueue {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        sqlx::query("INSERT INTO devices (id, name) VALUES ('dev-1', 'Phone')")
            .execute(&db.system)
            .await
            .unwrap();
        let q = SqliteNotificationQueue::new(db.system.clone());
        std::mem::forget(tmp);
        q
    }

    fn notif(id: &str, target: &str) -> Notification {
        Notification {
            id: id.into(),
            target: target.into(),
            category: "info".into(),
            title: "Hi".into(),
            body: "body".into(),
            timestamp: "2026-06-29T00:00:00Z".into(),
            data: Some(serde_json::json!({ "k": "v" })),
        }
    }

    #[tokio::test]
    async fn enqueue_list_mark_delivered() {
        let q = fresh().await;
        assert!(q.list_undelivered("dev-1").await.unwrap().is_empty());

        q.enqueue(notif("n1", "dev-1")).await.unwrap();
        q.enqueue(notif("n2", "dev-1")).await.unwrap();

        let undelivered = q.list_undelivered("dev-1").await.unwrap();
        assert_eq!(undelivered.len(), 2);
        assert_eq!(undelivered[0].id, "n1"); // oldest first
        assert_eq!(undelivered[0].data, Some(serde_json::json!({ "k": "v" })));

        q.mark_delivered(&["n1".to_string()]).await.unwrap();
        let left = q.list_undelivered("dev-1").await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "n2");

        // Idempotent / empty input is a no-op.
        q.mark_delivered(&[]).await.unwrap();
    }
}
