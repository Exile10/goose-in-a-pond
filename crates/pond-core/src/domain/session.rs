use crate::domain::message::ChatMessage;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single message within a session, including metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub id: String,
    pub session_id: String,
    pub message: ChatMessage,
    pub created_at: DateTime<Utc>,
}

impl SessionMessage {
    pub fn new(id: String, session_id: String, message: ChatMessage) -> Self {
        Self {
            id,
            session_id,
            message,
            created_at: Utc::now(),
        }
    }
}

/// Represents a conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Session {
    pub fn new(id: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            created_at: now,
            updated_at: now,
        }
    }
}
