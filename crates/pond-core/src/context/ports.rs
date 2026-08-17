//! Driven port: storage for personal context (PAI-8 P1).
//!
//! # Every method is required
//!
//! There is not one default body on this trait, and that is deliberate. PAI-2
//! P3's `RedactingMemoryRepository` had to carry a source-parsing guard
//! (`every_memory_repository_method_is_forwarded`) precisely because eighteen of
//! `MemoryRepository`'s twenty-two methods have default bodies: a decorator that
//! forgets one compiles, answers `Ok(0)`, and the row never reaches SQLite.
//! Recorded vacuity shape 7 is the same defect seen from the test side. With no
//! defaults, a decorator that forgets a method does not compile.
//!
//! # Every read takes a scope
//!
//! PAI-8 invariant 2 is "a `Guest` session sees no context items. None." A read
//! that took no scope would be the one call site that could not honour it, so
//! there is no such read here — not even `get`, which takes a scope for the same
//! reason `search_recent` does.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::context::domain::{ContextItem, ContextSource};
use crate::context::retention::ContextRetention;
use crate::user_data::domain::profile::ProfileScope;

#[async_trait]
pub trait ContextRepository: Send + Sync {
    /// Create or update a source.
    ///
    /// `kind` and `profile_id` are part of a source's identity and an
    /// implementation must not change them on conflict; migration 0044 refuses
    /// the update outright, because a source whose owner moved would
    /// misattribute every item already stored under it.
    async fn upsert_source(&self, source: &ContextSource) -> Result<()>;

    /// One source, if this scope may see it.
    async fn get_source(&self, id: &str, scope: &ProfileScope) -> Result<Option<ContextSource>>;

    /// Every source this scope may see.
    async fn list_sources(&self, scope: &ProfileScope) -> Result<Vec<ContextSource>>;

    /// Disconnect a source and delete its items. Returns how many items went
    /// with it.
    ///
    /// PAI-8 invariant 6 is "disconnecting a source deletes its items by
    /// default, **and says how many**". The count is the return value rather
    /// than a log line for that reason: a caller that cannot report the number
    /// cannot satisfy the invariant.
    async fn disconnect_source(&self, id: &str, scope: &ProfileScope) -> Result<u64>;

    /// Store an item, idempotently on `(source_id, external_id)`.
    ///
    /// Re-syncing a mailbox must update the row rather than duplicate it — a
    /// cursor that slips backwards is normal, and a store that answered it with
    /// duplicates would fill the corpus with the same three messages.
    async fn save_item(&self, item: &ContextItem) -> Result<()>;

    /// The newest items this scope may see.
    async fn recent_items(&self, scope: &ProfileScope, limit: usize) -> Result<Vec<ContextItem>>;

    /// Keyword fallback, for when no embedding provider is wired.
    async fn search_items(
        &self,
        keywords: &[String],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<ContextItem>>;

    /// Semantic search. Returns each item with its cosine similarity, so the
    /// caller can rank on the same blend memory uses rather than on similarity
    /// alone.
    async fn search_similar(
        &self,
        query_embedding: &[f32],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<(ContextItem, f32)>>;

    /// Items with no vector yet, for the background embedding backfill.
    async fn search_unembedded(&self, limit: usize) -> Result<Vec<ContextItem>>;

    /// Attach a vector to a stored item.
    async fn update_embedding(&self, id: &str, embedding: &[f32]) -> Result<()>;

    /// How many items a member owns. Used to say what deleting them removes.
    async fn count_for_profile(&self, profile_id: &str) -> Result<u64>;

    /// Delete everything past its retention window. Returns how many rows went.
    ///
    /// Takes the whole [`ContextRetention`] rather than a day count because the
    /// window depends on the item's source kind AND its sensitivity, and a
    /// caller that flattened those into one number would silently keep sensitive
    /// items for the baseline period.
    async fn purge_expired(&self, retention: &ContextRetention, now: DateTime<Utc>) -> Result<u64>;
}

// ── Asking an account for news, now ─────────────────────────────────────────

/// What one sync pass did, in the shape a person can be told.
///
/// Deliberately counts rather than a bare success flag: "checked, nothing new"
/// and "checked, found eleven things" are both successes and they are not the
/// same answer, and somebody who just pressed a button deserves to know which
/// one happened.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountSyncSummary {
    /// Accounts considered.
    pub sources: usize,
    /// Accounts whose upstream said nothing had changed.
    pub unchanged: usize,
    /// Items stored.
    pub ingested: usize,
    /// Accounts whose credentials were refused.
    pub needs_reauth: usize,
    /// Accounts that failed for some other reason.
    pub failed: usize,
    /// Accounts skipped because the pond is offline.
    pub paused: usize,
}

/// Pull every connected account now, rather than waiting for the timer.
///
/// A port because the sync itself composes a repository, a pipeline, a secret
/// store and one protocol adapter per kind — a combination that belongs to the
/// binary that wires them, not to a domain that has never heard of CalDAV. The
/// route only needs to be able to ASK.
#[async_trait]
pub trait AccountSync: Send + Sync {
    async fn sync_now(&self) -> Result<AccountSyncSummary>;
}
