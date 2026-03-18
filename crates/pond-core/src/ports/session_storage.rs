use crate::domain::session::{Session, SessionMessage};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SessionStorageError {
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Message not found: {0}")]
    MessageNotFound(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("General error: {0}")]
    General(String),
}

/// Driven Port: SessionStorage
///
/// This trait defines the interface for persisting conversation sessions and messages.
/// Implementations can range from in-memory storage to database-backed persistence.
#[async_trait::async_trait]
pub trait SessionStorage: Send + Sync {
    /// Create a new session.
    async fn create_session(&self, session_id: String) -> Result<Session, SessionStorageError>;

    /// Get a session by ID.
    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError>;

    /// Add a message to a session.
    async fn add_message(
        &self,
        session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError>;

    /// Get all messages for a session.
    async fn get_messages(&self, session_id: &str) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Delete a session and all its messages.
    async fn delete_session(&self, session_id: &str) -> Result<(), SessionStorageError>;
}
