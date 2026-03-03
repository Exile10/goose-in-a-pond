use crate::domain::session::{Session, SessionMessage};
use crate::ports::session_storage::{SessionStorage, SessionStorageError};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-memory session storage implementation for testing.
///
/// This mock implementation stores sessions and messages in memory using
/// a HashMap protected by RwLock for thread-safe concurrent access.
pub struct InMemorySessionStorage {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    messages: Arc<RwLock<HashMap<String, Vec<SessionMessage>>>>,
}

impl InMemorySessionStorage {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            messages: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for InMemorySessionStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SessionStorage for InMemorySessionStorage {
    async fn create_session(&self, session_id: String) -> Result<Session, SessionStorageError> {
        let session = Session::new(session_id.clone());
        self.sessions
            .write()
            .await
            .insert(session_id.clone(), session.clone());
        self.messages
            .write()
            .await
            .insert(session_id, Vec::new());
        Ok(session)
    }

    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError> {
        self.sessions
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionStorageError::SessionNotFound(session_id.to_string()))
    }

    async fn add_message(
        &self,
        session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError> {
        // Ensure session exists
        self.get_session(&session_id).await?;

        // Add message to the session
        let mut messages = self.messages.write().await;
        if let Some(msgs) = messages.get_mut(&session_id) {
            msgs.push(message.clone());
        } else {
            messages.insert(session_id, vec![message.clone()]);
        }

        Ok(message)
    }

    async fn get_messages(&self, session_id: &str) -> Result<Vec<SessionMessage>, SessionStorageError> {
        // Ensure session exists
        self._get_session(session_id).await?;

        Ok(self
            .messages
            .read()
            .await
            .get(session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn delete_session(&self, session_id: &str) -> Result<(), SessionStorageError> {
        self.sessions.write().await.remove(session_id);
        self.messages.write().await.remove(session_id);
        Ok(())
    }
}

impl InMemorySessionStorage {
    /// Internal helper method to check session existence without exposing get_session
    async fn _get_session(&self, session_id: &str) -> Result<(), SessionStorageError> {
        self.sessions
            .read()
            .await
            .contains_key(session_id)
            .then_some(())
            .ok_or_else(|| SessionStorageError::SessionNotFound(session_id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::message::{ChatMessage, Role};

    #[tokio::test]
    async fn test_create_session() {
        let storage = InMemorySessionStorage::new();
        let session = storage.create_session("session-1".to_string()).await.unwrap();
        assert_eq!(session.id, "session-1");
    }

    #[tokio::test]
    async fn test_get_session() {
        let storage = InMemorySessionStorage::new();
        storage.create_session("session-1".to_string()).await.unwrap();
        let retrieved = storage.get_session("session-1").await.unwrap();
        assert_eq!(retrieved.id, "session-1");
    }

    #[tokio::test]
    async fn test_get_nonexistent_session() {
        let storage = InMemorySessionStorage::new();
        let result = storage.get_session("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_add_message() {
        let storage = InMemorySessionStorage::new();
        storage.create_session("session-1".to_string()).await.unwrap();

        let message = ChatMessage::user("Hello");
        let session_message = SessionMessage::new(
            "msg-1".to_string(),
            "session-1".to_string(),
            message,
        );

        let result = storage
            .add_message("session-1".to_string(), session_message.clone())
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_messages() {
        let storage = InMemorySessionStorage::new();
        storage.create_session("session-1".to_string()).await.unwrap();

        let msg1 = ChatMessage::user("Hello");
        let session_msg1 = SessionMessage::new(
            "msg-1".to_string(),
            "session-1".to_string(),
            msg1,
        );

        let msg2 = ChatMessage::assistant("Hi there");
        let session_msg2 = SessionMessage::new(
            "msg-2".to_string(),
            "session-1".to_string(),
            msg2,
        );

        storage
            .add_message("session-1".to_string(), session_msg1)
            .await
            .unwrap();
        storage
            .add_message("session-1".to_string(), session_msg2)
            .await
            .unwrap();

        let messages = storage.get_messages("session-1").await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message.role, Role::User);
        assert_eq!(messages[1].message.role, Role::Assistant);
    }

    #[tokio::test]
    async fn test_delete_session() {
        let storage = InMemorySessionStorage::new();
        storage.create_session("session-1".to_string()).await.unwrap();

        let result = storage.delete_session("session-1").await;
        assert!(result.is_ok());

        let get_result = storage.get_session("session-1").await;
        assert!(get_result.is_err());
    }

    #[tokio::test]
    async fn test_messages_persist_across_iterations() {
        let storage = InMemorySessionStorage::new();
        let session_id = "session-1".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        // First iteration: add a user message
        let user_msg = ChatMessage::user("First message");
        let session_msg1 = SessionMessage::new(
            "msg-1".to_string(),
            session_id.clone(),
            user_msg,
        );
        storage
            .add_message(session_id.clone(), session_msg1)
            .await
            .unwrap();

        // Second iteration: add an assistant response
        let assistant_msg = ChatMessage::assistant("First response");
        let session_msg2 = SessionMessage::new(
            "msg-2".to_string(),
            session_id.clone(),
            assistant_msg,
        );
        storage
            .add_message(session_id.clone(), session_msg2)
            .await
            .unwrap();

        // Verify both messages persist
        let messages = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message.content, "First message");
        assert_eq!(messages[1].message.content, "First response");
    }
}
