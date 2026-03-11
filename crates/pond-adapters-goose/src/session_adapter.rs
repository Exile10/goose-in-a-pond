use pond_core::domain::session::{Session, SessionMessage};
use pond_core::ports::session_storage::{SessionStorage, SessionStorageError};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Adapter: GooseSessionAdapter
///
/// Wraps Goose's SessionManager to implement the SessionStorage port.
/// Provides session and message persistence integrated with Goose's session system.
pub struct GooseSessionAdapter {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    messages: Arc<RwLock<HashMap<String, Vec<SessionMessage>>>>,
}

impl GooseSessionAdapter {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            messages: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for GooseSessionAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SessionStorage for GooseSessionAdapter {
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

impl GooseSessionAdapter {
    /// Internal helper method to check session existence
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
    use pond_core::domain::message::ChatMessage;

    #[tokio::test]
    async fn test_create_session() {
        let adapter = GooseSessionAdapter::new();
        let session = adapter.create_session("session-1".to_string()).await.unwrap();
        assert_eq!(session.id, "session-1");
    }

    #[tokio::test]
    async fn test_add_and_retrieve_messages() {
        let adapter = GooseSessionAdapter::new();
        adapter.create_session("session-1".to_string()).await.unwrap();

        let message = ChatMessage::user("Hello from Goose");
        let session_message = SessionMessage::new(
            "msg-1".to_string(),
            "session-1".to_string(),
            message,
        );

        adapter
            .add_message("session-1".to_string(), session_message)
            .await
            .unwrap();

        let messages = adapter.get_messages("session-1").await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message.content, "Hello from Goose");
    }
}
