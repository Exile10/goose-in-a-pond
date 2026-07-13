use crate::user_data::domain::session::{Session, SessionMessage};
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
    async fn get_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Update the title of a session.
    async fn update_title(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<(), SessionStorageError>;

    /// Delete a session and all its messages.
    async fn delete_session(&self, session_id: &str) -> Result<(), SessionStorageError>;

    /// List all sessions, ordered by most recently updated first.
    async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError>;

    /// Get messages for a session with pagination.
    ///
    /// Returns up to `limit` messages starting from `offset`, ordered chronologically.
    async fn get_messages_paginated(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Fetch the most recent `limit` messages, returned in chronological order
    /// (oldest-first). Use this instead of `get_messages()` to cap context load.
    async fn get_recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Increment the cumulative token usage for a session.
    async fn increment_usage(
        &self,
        _session_id: &str,
        _prompt_tokens: u32,
        _completion_tokens: u32,
        _model_name: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    /// Count the messages stored for a session.
    ///
    /// Used to render a per-conversation badge in the history sidebar.
    /// The default returns 0 so in-memory mocks and legacy adapters keep
    /// compiling; real adapters override with an indexed `COUNT(*)`.
    async fn count_messages(&self, _session_id: &str) -> Result<u64, SessionStorageError> {
        Ok(0) // default no-op for backward compat
    }

    /// Return the content of the earliest user message in a session, if any.
    ///
    /// Used as a read-time fallback to derive a human-readable label when a
    /// session has no stored `title`. The default returns `None` so mocks and
    /// legacy adapters keep compiling.
    async fn first_user_message(
        &self,
        _session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None) // default no-op for backward compat
    }
}
