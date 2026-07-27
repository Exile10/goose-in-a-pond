//! SQLite-backed implementation of the SessionStorage port.
//!
//! Wraps `Pool<Sqlite>` pointing at `pond_system.db`.
//! Tables are created by `migrations/system/0001_initial.sql`.

use async_trait::async_trait;
use pond_core::models::domain::message::{ChatMessage, Role, ToolCallRecord};
use pond_core::user_data::domain::session::{Session, SessionMessage};
use pond_core::user_data::ports::session_storage::{SessionStorage, SessionStorageError};
use sqlx::{Pool, Row, Sqlite};

// ── Raw DB row types ──────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    title: Option<String>,
    total_prompt_tokens: i64,
    total_completion_tokens: i64,
    model_name: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: String,
    session_id: String,
    role: String,
    content: String,
    tool_call_id: Option<String>,
    tool_calls_json: Option<String>,
    created_at: String,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
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
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
}

fn str_to_role(s: &str) -> Result<Role, SessionStorageError> {
    match s {
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "system" => Ok(Role::System),
        "tool" => Ok(Role::Tool),
        other => Err(SessionStorageError::StorageError(format!(
            "Unknown role in DB: '{}'",
            other
        ))),
    }
}

impl TryFrom<SessionRow> for Session {
    type Error = SessionStorageError;
    fn try_from(r: SessionRow) -> Result<Self, Self::Error> {
        Ok(Session {
            id: r.id,
            title: r.title,
            total_prompt_tokens: r.total_prompt_tokens as u32,
            total_completion_tokens: r.total_completion_tokens as u32,
            model_name: r.model_name,
            created_at: parse_dt(&r.created_at),
            updated_at: parse_dt(&r.updated_at),
        })
    }
}

impl TryFrom<MessageRow> for SessionMessage {
    type Error = SessionStorageError;
    fn try_from(r: MessageRow) -> Result<Self, Self::Error> {
        // Tool-call metadata is stored as JSON. Malformed JSON degrades gracefully:
        // we drop the tool_calls but keep the message rather than failing the read.
        let tool_calls: Vec<ToolCallRecord> = r
            .tool_calls_json
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        Ok(SessionMessage {
            id: r.id,
            session_id: r.session_id,
            message: ChatMessage {
                role: str_to_role(&r.role)?,
                content: r.content,
                images: Vec::new(),
                tool_calls,
                tool_call_id: r.tool_call_id,
            },
            created_at: parse_dt(&r.created_at),
            prompt_tokens: r.prompt_tokens.map(|v| v as u32),
            completion_tokens: r.completion_tokens.map(|v| v as u32),
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
            "INSERT INTO sessions (id, title, created_at, updated_at) \
             VALUES (?, NULL, datetime('now'), datetime('now'))",
        )
        .bind(&session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        self.get_session(&session_id).await
    }

    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError> {
        let row = sqlx::query_as::<_, SessionRow>(
            "SELECT id, title, total_prompt_tokens, total_completion_tokens, model_name, created_at, updated_at FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        match row {
            Some(r) => Session::try_from(r),
            None => Err(SessionStorageError::SessionNotFound(session_id.to_string())),
        }
    }

    async fn get_rolling_summary(
        &self,
        session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        let row = sqlx::query(
            "SELECT rolling_summary, rolling_summary_through_id FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(row
            .map(|r| {
                (
                    r.get("rolling_summary"),
                    r.get("rolling_summary_through_id"),
                )
            })
            .unwrap_or((None, None)))
    }

    async fn set_rolling_summary(
        &self,
        session_id: &str,
        summary: &str,
        through_message_id: &str,
    ) -> Result<(), SessionStorageError> {
        sqlx::query(
            "UPDATE sessions SET rolling_summary = ?, rolling_summary_through_id = ?, \
             rolling_summary_updated_at = datetime('now') WHERE id = ?",
        )
        .bind(summary)
        .bind(through_message_id)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn get_engine_session_id(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        let row =
            sqlx::query("SELECT engine_session_id FROM engine_session_map WHERE session_id = ?")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(row.map(|r| r.get("engine_session_id")))
    }

    async fn set_engine_session_id(
        &self,
        session_id: &str,
        engine_session_id: &str,
    ) -> Result<(), SessionStorageError> {
        // No `sessions` existence guard on purpose — see the port doc-comment:
        // the pairing can be established before a GIAP session row exists.
        sqlx::query(
            "INSERT INTO engine_session_map (session_id, engine_session_id, updated_at) \
             VALUES (?, ?, datetime('now')) \
             ON CONFLICT(session_id) DO UPDATE SET \
               engine_session_id = excluded.engine_session_id, \
               updated_at = excluded.updated_at",
        )
        .bind(session_id)
        .bind(engine_session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn get_session_tool_groups(
        &self,
        session_id: &str,
    ) -> Result<Option<Vec<String>>, SessionStorageError> {
        let row = sqlx::query("SELECT groups FROM session_tool_groups WHERE session_id = ?")
            .bind(session_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(row.map(|r| {
            let raw: String = r.get("groups");
            raw.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        }))
    }

    async fn set_session_tool_groups(
        &self,
        session_id: &str,
        groups: &[String],
    ) -> Result<(), SessionStorageError> {
        // No `sessions` existence guard on purpose — see the port doc-comment.
        sqlx::query(
            "INSERT INTO session_tool_groups (session_id, groups, updated_at) \
             VALUES (?, ?, datetime('now')) \
             ON CONFLICT(session_id) DO UPDATE SET \
               groups = excluded.groups, \
               updated_at = excluded.updated_at",
        )
        .bind(session_id)
        .bind(groups.join("\n"))
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn add_message(
        &self,
        session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError> {
        self.get_session(&session_id).await?; // guard: session must exist

        // Serialize tool_calls only when present — keeps storage compact for
        // the common user/system path.
        let tool_calls_json = if message.message.tool_calls.is_empty() {
            None
        } else {
            serde_json::to_string(&message.message.tool_calls).ok()
        };

        sqlx::query(
            "INSERT INTO session_messages \
                 (id, session_id, role, content, tool_call_id, tool_calls_json, created_at, \
                  prompt_tokens, completion_tokens) \
             VALUES (?, ?, ?, ?, ?, ?, datetime('now'), ?, ?)",
        )
        .bind(&message.id)
        .bind(&session_id)
        .bind(role_to_str(&message.message.role))
        .bind(&message.message.content)
        .bind(&message.message.tool_call_id)
        .bind(&tool_calls_json)
        .bind(message.prompt_tokens.map(|v| v as i64))
        .bind(message.completion_tokens.map(|v| v as i64))
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

    async fn get_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        let rows = sqlx::query_as::<_, MessageRow>(
            "SELECT id, session_id, role, content, tool_call_id, tool_calls_json, created_at, \
             prompt_tokens, completion_tokens \
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

    async fn update_title(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<(), SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        sqlx::query("UPDATE sessions SET title = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(&title)
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(())
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

    async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError> {
        let rows = sqlx::query_as::<_, SessionRow>(
            "SELECT id, title, total_prompt_tokens, total_completion_tokens, model_name, created_at, updated_at FROM sessions ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        rows.into_iter().map(Session::try_from).collect()
    }

    async fn get_messages_paginated(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        let rows = sqlx::query_as::<_, MessageRow>(
            "SELECT id, session_id, role, content, tool_call_id, tool_calls_json, created_at, \
             prompt_tokens, completion_tokens \
             FROM session_messages \
             WHERE session_id = ? \
             ORDER BY created_at ASC, rowid ASC \
             LIMIT ? OFFSET ?",
        )
        .bind(session_id)
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        rows.into_iter().map(SessionMessage::try_from).collect()
    }

    async fn get_recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        // Fetch newest-first, then reverse to return chronological order.
        let rows = sqlx::query_as::<_, MessageRow>(
            "SELECT id, session_id, role, content, tool_call_id, tool_calls_json, created_at, \
             prompt_tokens, completion_tokens \
             FROM session_messages \
             WHERE session_id = ? \
             ORDER BY created_at DESC, rowid DESC \
             LIMIT ?",
        )
        .bind(session_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        let mut messages: Vec<SessionMessage> = rows
            .into_iter()
            .map(SessionMessage::try_from)
            .collect::<Result<_, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    async fn increment_usage(
        &self,
        session_id: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        model_name: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        sqlx::query(
            "UPDATE sessions SET \
                total_prompt_tokens = total_prompt_tokens + ?, \
                total_completion_tokens = total_completion_tokens + ?, \
                model_name = COALESCE(?, model_name), \
                updated_at = datetime('now') \
             WHERE id = ?",
        )
        .bind(prompt_tokens as i64)
        .bind(completion_tokens as i64)
        .bind(model_name)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn count_messages(&self, session_id: &str) -> Result<u64, SessionStorageError> {
        // Indexed COUNT(*) — cheap even for long conversations. Unlike the
        // read methods, this deliberately does NOT guard on session existence:
        // a missing session simply has zero messages, which is the answer the
        // sidebar badge wants.
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE session_id = ?")
                .bind(session_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(count.max(0) as u64)
    }

    async fn first_user_message(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        // Earliest user-authored message, used only as a read-time title
        // fallback. Ordered identically to get_messages so "first" is stable.
        let content: Option<String> = sqlx::query_scalar(
            "SELECT content FROM session_messages \
             WHERE session_id = ? AND role = 'user' \
             ORDER BY created_at ASC, rowid ASC \
             LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(content)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use pond_core::models::domain::message::ChatMessage;
    use tempfile::tempdir;

    async fn make_storage() -> (SqliteSessionStorage, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (SqliteSessionStorage::new(db.system), tmp)
    }

    #[tokio::test]
    async fn create_and_get_session() {
        let (s, _tmp) = make_storage().await;
        let session = s.create_session("sess-1".to_string()).await.unwrap();
        assert_eq!(session.id, "sess-1");
        let fetched = s.get_session("sess-1").await.unwrap();
        assert_eq!(fetched.id, "sess-1");
    }

    /// C1: the engine-session pairing must survive a process restart, which is
    /// what a fresh storage handle over the same file simulates.
    #[tokio::test]
    async fn engine_session_pairing_persists_and_upserts() {
        let tmp = tempdir().unwrap();
        {
            let db = Database::init(tmp.path()).await.unwrap();
            let s = SqliteSessionStorage::new(db.system);
            assert_eq!(s.get_engine_session_id("sess-1").await.unwrap(), None);
            // No sessions row on purpose — the pairing must not require one.
            s.set_engine_session_id("sess-1", "20260728_1")
                .await
                .unwrap();
            assert_eq!(
                s.get_engine_session_id("sess-1").await.unwrap().as_deref(),
                Some("20260728_1")
            );
            // Re-pairing (engine store wiped) overwrites rather than erroring.
            s.set_engine_session_id("sess-1", "20260728_9")
                .await
                .unwrap();
            assert_eq!(
                s.get_engine_session_id("sess-1").await.unwrap().as_deref(),
                Some("20260728_9")
            );
        }
        let db = Database::init(tmp.path()).await.unwrap();
        let reopened = SqliteSessionStorage::new(db.system);
        assert_eq!(
            reopened
                .get_engine_session_id("sess-1")
                .await
                .unwrap()
                .as_deref(),
            Some("20260728_9"),
            "pairing must survive a restart"
        );
        assert_eq!(reopened.get_engine_session_id("other").await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_nonexistent_session_returns_error() {
        let (s, _tmp) = make_storage().await;
        let result = s.get_session("missing").await;
        assert!(matches!(
            result,
            Err(SessionStorageError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn add_and_retrieve_messages() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        let msg = SessionMessage::new(
            "m1".to_string(),
            "sess-1".to_string(),
            ChatMessage::user("Hello"),
        );
        s.add_message("sess-1".to_string(), msg).await.unwrap();
        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].message.content, "Hello");
        assert_eq!(msgs[0].message.role, Role::User);
    }

    #[tokio::test]
    async fn messages_preserve_insertion_order() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-1".to_string(),
                ChatMessage::user("First"),
            ),
        )
        .await
        .unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m2".to_string(),
                "sess-1".to_string(),
                ChatMessage::assistant("Second"),
            ),
        )
        .await
        .unwrap();
        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs[0].message.content, "First");
        assert_eq!(msgs[1].message.content, "Second");
    }

    #[tokio::test]
    async fn delete_session_cascades_to_messages() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-1".to_string(),
                ChatMessage::user("Hi"),
            ),
        )
        .await
        .unwrap();
        s.delete_session("sess-1").await.unwrap();
        assert!(matches!(
            s.get_session("sess-1").await,
            Err(SessionStorageError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn add_message_to_missing_session_errors() {
        let (s, _tmp) = make_storage().await;
        let msg = SessionMessage::new(
            "m1".to_string(),
            "no-session".to_string(),
            ChatMessage::user("Hi"),
        );
        let result = s.add_message("no-session".to_string(), msg).await;
        assert!(matches!(
            result,
            Err(SessionStorageError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn list_sessions_returns_all_ordered() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-a".to_string()).await.unwrap();
        s.create_session("sess-b".to_string()).await.unwrap();
        // Add a message to sess-a to update its updated_at
        s.add_message(
            "sess-a".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-a".to_string(),
                ChatMessage::user("Hi"),
            ),
        )
        .await
        .unwrap();

        let sessions = s.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
        // sess-a was updated more recently, so it should be first
        assert_eq!(sessions[0].id, "sess-a");
        assert_eq!(sessions[1].id, "sess-b");
    }

    #[tokio::test]
    async fn get_messages_paginated_works() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        for i in 0..10 {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(
                    format!("m{}", i),
                    "sess-1".to_string(),
                    ChatMessage::user(format!("Msg {}", i)),
                ),
            )
            .await
            .unwrap();
        }

        let page = s.get_messages_paginated("sess-1", 3, 0).await.unwrap();
        assert_eq!(page.len(), 3);
        assert_eq!(page[0].message.content, "Msg 0");

        let page2 = s.get_messages_paginated("sess-1", 3, 7).await.unwrap();
        assert_eq!(page2.len(), 3);
        assert_eq!(page2[0].message.content, "Msg 7");

        let past_end = s.get_messages_paginated("sess-1", 5, 100).await.unwrap();
        assert!(past_end.is_empty());
    }

    #[tokio::test]
    async fn session_title_defaults_to_none() {
        let (s, _tmp) = make_storage().await;
        let session = s.create_session("sess-1".to_string()).await.unwrap();
        assert_eq!(session.title, None);
    }

    #[tokio::test]
    async fn update_title_sets_and_persists() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();

        s.update_title("sess-1", "Weather Chat".to_string())
            .await
            .unwrap();
        let session = s.get_session("sess-1").await.unwrap();
        assert_eq!(session.title, Some("Weather Chat".to_string()));
    }

    #[tokio::test]
    async fn update_title_on_missing_session_errors() {
        let (s, _tmp) = make_storage().await;
        let result = s.update_title("missing", "Nope".to_string()).await;
        assert!(matches!(
            result,
            Err(SessionStorageError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn get_recent_messages_returns_newest_in_order() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        for i in 0..10 {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(
                    format!("m{}", i),
                    "sess-1".to_string(),
                    ChatMessage::user(format!("Msg {}", i)),
                ),
            )
            .await
            .unwrap();
        }

        // Ask for 3 most recent → should get Msg 7, 8, 9 in chronological order
        let recent = s.get_recent_messages("sess-1", 3).await.unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].message.content, "Msg 7");
        assert_eq!(recent[1].message.content, "Msg 8");
        assert_eq!(recent[2].message.content, "Msg 9");
    }

    #[tokio::test]
    async fn get_recent_messages_limit_exceeds_count_returns_all() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        for i in 0..3 {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(
                    format!("m{}", i),
                    "sess-1".to_string(),
                    ChatMessage::user(format!("Msg {}", i)),
                ),
            )
            .await
            .unwrap();
        }

        let recent = s.get_recent_messages("sess-1", 100).await.unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].message.content, "Msg 0");
    }

    #[tokio::test]
    async fn tool_call_metadata_round_trips() {
        use pond_core::models::domain::message::ToolCallRecord;
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();

        let asst = SessionMessage::new(
            "m-asst".to_string(),
            "sess-1".to_string(),
            ChatMessage::assistant_with_tool_calls(
                "calling weather",
                vec![ToolCallRecord {
                    id: "call-42".to_string(),
                    name: "get_weather".to_string(),
                    arguments: "{\"city\":\"Nairobi\"}".to_string(),
                }],
            ),
        );
        s.add_message("sess-1".to_string(), asst).await.unwrap();

        let tool = SessionMessage::new(
            "m-tool".to_string(),
            "sess-1".to_string(),
            ChatMessage::tool_result("sunny, 24C", "call-42"),
        );
        s.add_message("sess-1".to_string(), tool).await.unwrap();

        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].message.tool_calls.len(), 1);
        assert_eq!(msgs[0].message.tool_calls[0].id, "call-42");
        assert_eq!(msgs[0].message.tool_calls[0].name, "get_weather");
        assert_eq!(msgs[1].message.role, Role::Tool);
        assert_eq!(msgs[1].message.tool_call_id.as_deref(), Some("call-42"));
    }

    #[tokio::test]
    async fn count_messages_reflects_stored_rows() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        assert_eq!(s.count_messages("sess-1").await.unwrap(), 0);

        for i in 0..5 {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(
                    format!("m{}", i),
                    "sess-1".to_string(),
                    ChatMessage::user(format!("Msg {}", i)),
                ),
            )
            .await
            .unwrap();
        }
        assert_eq!(s.count_messages("sess-1").await.unwrap(), 5);
    }

    #[tokio::test]
    async fn count_messages_missing_session_is_zero() {
        let (s, _tmp) = make_storage().await;
        // No error, no session — just zero rows.
        assert_eq!(s.count_messages("nope").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn first_user_message_returns_earliest_user_row() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        // No user messages yet.
        assert_eq!(s.first_user_message("sess-1").await.unwrap(), None);

        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m0".to_string(),
                "sess-1".to_string(),
                ChatMessage::assistant("greeting"),
            ),
        )
        .await
        .unwrap();
        // Still no *user* message.
        assert_eq!(s.first_user_message("sess-1").await.unwrap(), None);

        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-1".to_string(),
                ChatMessage::user("What is the weather in Nairobi today?"),
            ),
        )
        .await
        .unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m2".to_string(),
                "sess-1".to_string(),
                ChatMessage::user("second question"),
            ),
        )
        .await
        .unwrap();

        assert_eq!(
            s.first_user_message("sess-1").await.unwrap(),
            Some("What is the weather in Nairobi today?".to_string())
        );
    }

    #[tokio::test]
    async fn messages_survive_restart() {
        let tmp = tempdir().unwrap();
        // First run: write
        {
            let db = Database::init(tmp.path()).await.unwrap();
            let s = SqliteSessionStorage::new(db.system);
            s.create_session("persistent".to_string()).await.unwrap();
            s.add_message(
                "persistent".to_string(),
                SessionMessage::new(
                    "m1".to_string(),
                    "persistent".to_string(),
                    ChatMessage::user("Remember me"),
                ),
            )
            .await
            .unwrap();
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

    #[tokio::test]
    async fn token_counts_round_trip_and_default_null() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let s = SqliteSessionStorage::new(db.system);
        s.create_session("tok".to_string()).await.unwrap();

        s.add_message(
            "tok".to_string(),
            SessionMessage::new(
                "u1".to_string(),
                "tok".to_string(),
                ChatMessage::user("question"),
            ),
        )
        .await
        .unwrap();
        s.add_message(
            "tok".to_string(),
            SessionMessage::new(
                "a1".to_string(),
                "tok".to_string(),
                ChatMessage::assistant("answer"),
            )
            .with_token_counts(Some(1930), Some(87)),
        )
        .await
        .unwrap();

        let msgs = s.get_messages("tok").await.unwrap();
        assert_eq!(msgs[0].prompt_tokens, None, "user rows carry no counts");
        assert_eq!(msgs[1].prompt_tokens, Some(1930));
        assert_eq!(msgs[1].completion_tokens, Some(87));
    }
}
