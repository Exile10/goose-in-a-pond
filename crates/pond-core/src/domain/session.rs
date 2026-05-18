use crate::domain::message::ChatMessage;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single message within a session, including metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: Option<String>,
    /// Cumulative prompt tokens across all messages in this session.
    #[serde(default)]
    pub total_prompt_tokens: u32,
    /// Cumulative completion tokens across all messages in this session.
    #[serde(default)]
    pub total_completion_tokens: u32,
    /// The model most recently used in this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Session {
    pub fn new(id: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            title: None,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            model_name: None,
            created_at: now,
            updated_at: now,
        }
    }
}
