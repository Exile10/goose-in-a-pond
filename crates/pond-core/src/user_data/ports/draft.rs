//! Port for persisting and managing drafts (staged destructive actions).

use crate::user_data::domain::draft::{Draft, DraftStatus};
use anyhow::Result;
use async_trait::async_trait;

/// Repository for draft CRUD operations.
#[async_trait]
pub trait DraftRepository: Send + Sync {
    /// Persist a new draft.
    async fn save(&self, draft: Draft) -> Result<()>;

    /// List all pending drafts for a session.
    async fn list_pending(&self, session_id: &str) -> Result<Vec<Draft>>;

    /// Fetch a single draft by ID.
    async fn get(&self, id: &str) -> Result<Option<Draft>>;

    /// Transition a draft to a new status.
    async fn update_status(&self, id: &str, status: DraftStatus) -> Result<()>;
}
