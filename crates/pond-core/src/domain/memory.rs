//! Memory fragment domain type — stores snippets of conversation or facts
//! with optional embedding vectors for semantic search.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A persisted memory fragment.
///
/// `embedding` is stored as a raw f32 BLOB in SQLite and is `#[serde(skip)]`
/// so it never appears in JSON responses (it's binary data, not user-facing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFragment {
    pub id: String,
    /// Profile this memory belongs to (None = global)
    pub profile_id: Option<String>,
    /// Session this memory was extracted from (None = manual/external)
    pub session_id: Option<String>,
    /// The text content of the memory
    pub content: String,
    /// Raw embedding vector (None until an EmbeddingProvider generates it)
    #[serde(skip)]
    pub embedding: Option<Vec<f32>>,
    /// Source of this fragment: "chat", "note", "sensor_summary"
    pub source: String,
    /// Optional tags for categorization
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl MemoryFragment {
    /// Create a fragment sourced from a chat exchange.
    pub fn from_chat(
        id: String,
        profile_id: Option<String>,
        session_id: Option<String>,
        content: String,
    ) -> Self {
        Self {
            id,
            profile_id,
            session_id,
            content,
            embedding: None,
            source: "chat".to_string(),
            tags: vec![],
            created_at: Utc::now(),
        }
    }
}
