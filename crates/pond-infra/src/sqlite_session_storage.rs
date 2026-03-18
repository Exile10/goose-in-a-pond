//! SQLite-backed implementation of the SessionStorage port.
//!
//! Wraps `Pool<Sqlite>` pointing at `pond_system.db`.
//! Tables are created by `migrations/system/0001_initial.sql`.

use async_trait::async_trait;
use pond_core::domain::message::{ChatMessage, Role};
use pond_core::domain::session::{Session, SessionMessage};
use pond_core::ports::session_storage::{SessionStorage, SessionStorageError};
use sqlx::{Pool, Sqlite};

// ── Raw DB row types ──────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct SessionRow {
    id:         String,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct MessageRow {
    id:         String,
    session_id: String,
    role:       String,
    content:    String,
    created_at: String,
}

// ── Conversion helpers ────────────────────────────────────────────────────────

/// Parse SQLite's `datetime('now')` format ("YYYY-MM-DD HH:MM:SS") to DateTime<Utc>.
fn parse_dt(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or_else(|_| chrono::Utc::now())
}

fn role_to_str(role: &Role) -> &'static str {
    match role {
        Role::User      => "user",
        Role::Assistant => "assistant",
        Role::System    => "system",
    }
}

fn str_to_role(s: &str) -> Result<Role, SessionStorageError> {
    match s {
        "user"      => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "system"    => Ok(Role::System),
        other       => Err(SessionStorageError::StorageError(
            format!("Unknown role in DB: '{}'", other),
        )),
    }
}

impl TryFrom<SessionRow> for Session {
    type Error = SessionStorageError;
    fn try_from(r: SessionRow) -> Result<Self, Self::Error> {
        Ok(Session {
            id:         r.id,
            created_at: parse_dt(&r.created_at),
            updated_at: parse_dt(&r.updated_at),
        })
    }
}

impl TryFrom<MessageRow> for SessionMessage {
    type Error = SessionStorageError;
    fn try_from(r: MessageRow) -> Result<Self, Self::Error> {
        Ok(SessionMessage {
            id:         r.id,
            session_id: r.session_id,
            message:    ChatMessage { role: str_to_role(&r.role)?, content: r.content },
            created_at: parse_dt(&r.created_at),
        })
    }
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct SqliteSessionStorage {
    pool: Pool<Sqlite>,
}

impl SqliteSessionStorage {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SessionStorage for SqliteSessionStorage {
    async fn create_session(&self, session_id: String) -> Result<Session, SessionStorageError> {
        sqlx::query(
            "INSERT INTO sessions (id, created_at, updated_at) \
             VALUES (?, datetime('now'), datetime('now'))",
        )
        .bind(&session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        self.get_session(&session_id).await
    }

    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError> {
        let row = sqlx::query_as::<_, SessionRow>(
            "SELECT id, created_at, updated_at FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        match row {
            Some(r) => Session::try_from(r),
            None    => Err(SessionStorageError::SessionNotFound(session_id.to_string())),
        }
    }

    async fn add_message(
        &self,
        session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError> {
        self.get_session(&session_id).await?; // guard: session must exist

        sqlx::query(
            "INSERT INTO session_messages (id, session_id, role, content, created_at) \
             VALUES (?, ?, ?, ?, datetime('now'))",
        )
        .bind(&message.id)
        .bind(&session_id)
        .bind(role_to_str(&message.message.role))
        .bind(&message.message.content)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        sqlx::query("UPDATE sessions SET updated_at = datetime('now') WHERE id = ?")
            .bind(&session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(message)
    }

    async fn get_messages(&self, session_id: &str) -> Result<Vec<SessionMessage>, SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        let rows = sqlx::query_as::<_, MessageRow>(
            "SELECT id, session_id, role, content, created_at \
             FROM session_messages \
             WHERE session_id = ? \
             ORDER BY created_at ASC, rowid ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        rows.into_iter().map(SessionMessage::try_from).collect()
    }

    async fn delete_session(&self, session_id: &str) -> Result<(), SessionStorageError> {
        // ON DELETE CASCADE handles session_messages automatically
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use pond_core::domain::message::ChatMessage;
    use tempfile::tempdir;

    async fn make_storage() -> SqliteSessionStorage {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        SqliteSessionStorage::new(db.system)
    }

    #[tokio::test]
    async fn create_and_get_session() {
        let s = make_storage().await;
        let session = s.create_session("sess-1".to_string()).await.unwrap();
        assert_eq!(session.id, "sess-1");
        let fetched = s.get_session("sess-1").await.unwrap();
        assert_eq!(fetched.id, "sess-1");
    }

    #[tokio::test]
    async fn get_nonexistent_session_returns_error() {
        let s = make_storage().await;
        let result = s.get_session("missing").await;
        assert!(matches!(result, Err(SessionStorageError::SessionNotFound(_))));
    }

    #[tokio::test]
    async fn add_and_retrieve_messages() {
        let s = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        let msg = SessionMessage::new("m1".to_string(), "sess-1".to_string(), ChatMessage::user("Hello"));
        s.add_message("sess-1".to_string(), msg).await.unwrap();
        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].message.content, "Hello");
        assert_eq!(msgs[0].message.role, Role::User);
    }

    #[tokio::test]
    async fn messages_preserve_insertion_order() {
        let s = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        s.add_message("sess-1".to_string(), SessionMessage::new("m1".to_string(), "sess-1".to_string(), ChatMessage::user("First"))).await.unwrap();
        s.add_message("sess-1".to_string(), SessionMessage::new("m2".to_string(), "sess-1".to_string(), ChatMessage::assistant("Second"))).await.unwrap();
        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs[0].message.content, "First");
        assert_eq!(msgs[1].message.content, "Second");
    }

    #[tokio::test]
    async fn delete_session_cascades_to_messages() {
        let s = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        s.add_message("sess-1".to_string(), SessionMessage::new("m1".to_string(), "sess-1".to_string(), ChatMessage::user("Hi"))).await.unwrap();
        s.delete_session("sess-1").await.unwrap();
        assert!(matches!(s.get_session("sess-1").await, Err(SessionStorageError::SessionNotFound(_))));
    }

    #[tokio::test]
    async fn add_message_to_missing_session_errors() {
        let s = make_storage().await;
        let msg = SessionMessage::new("m1".to_string(), "no-session".to_string(), ChatMessage::user("Hi"));
        let result = s.add_message("no-session".to_string(), msg).await;
        assert!(matches!(result, Err(SessionStorageError::SessionNotFound(_))));
    }

    #[tokio::test]
    async fn messages_survive_restart() {
        let tmp = tempdir().unwrap();
        // First run: write
        {
            let db = Database::init(tmp.path()).await.unwrap();
            let s = SqliteSessionStorage::new(db.system);
            s.create_session("persistent".to_string()).await.unwrap();
            s.add_message("persistent".to_string(), SessionMessage::new("m1".to_string(), "persistent".to_string(), ChatMessage::user("Remember me"))).await.unwrap();
        }
        // Second run: read back
        {
            let db = Database::init(tmp.path()).await.unwrap();
            let s = SqliteSessionStorage::new(db.system);
            let msgs = s.get_messages("persistent").await.unwrap();
            assert_eq!(msgs.len(), 1);
            assert_eq!(msgs[0].message.content, "Remember me");
        }
    }
}
