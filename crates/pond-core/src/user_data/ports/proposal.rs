//! Port for persisting proactive proposals — PAI-7 P3.
//!
//! A proposal is stored in the `drafts` table, not in a table of its own. That
//! is section 3.2's decision and it is worth restating where the trait lives:
//! reusing the draft system means a proactive suggestion inherits an approval
//! flow that is already built, already understood and already trusted, rather
//! than a second confirmation mechanism users would have to learn.
//!
//! It is a separate trait from
//! [`DraftRepository`](crate::user_data::ports::draft::DraftRepository) rather
//! than four more methods on it, because the two have different read shapes: a
//! draft is scoped by the session that staged it, and a proposal has no session
//! at all — it is scoped by the member it is addressed to and by whether it is
//! still live.
//!
//! # Every read takes a `now`
//!
//! Invariant 7 is enforced on the READ, not by a sweeper. An expiry that only a
//! background task honours is one that stops holding the moment the task fails
//! to start — and PAI-7 P4 owns the only loop that could run it. Passing the
//! clock in makes the filter unskippable and makes it testable without waiting.
//! [`expire_due`](ProposalRepository::expire_due) exists to keep the table tidy
//! and to give the row a terminal status the feedback loop can read; nothing's
//! correctness depends on it running.

use crate::user_data::domain::proposal::Proposal;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Repository for proactive proposals, stored as rows in the `drafts` table.
#[async_trait]
pub trait ProposalRepository: Send + Sync {
    /// Persist a new proposal in the pending state.
    async fn save(&self, proposal: &Proposal) -> Result<()>;

    /// The live, still-pending proposals addressed to one household member,
    /// newest first.
    ///
    /// "Live" is doing real work: a proposal past its `expires_at` is not
    /// returned, whether or not anything ever swept it. Invariant 4 is the
    /// signature — there is no "list every proposal" method, because the only
    /// caller that could want one is a broadcast.
    async fn list_live_for(&self, profile_id: &str, now: DateTime<Utc>) -> Result<Vec<Proposal>>;

    /// One proposal by id, if it is still pending and still live.
    ///
    /// Returns `Ok(None)` for expired, decided and absent alike. A caller that
    /// needs to tell those apart is deciding something, and deciding goes
    /// through the draft path, which has the ownership gate.
    async fn get_live(&self, id: &str, now: DateTime<Utc>) -> Result<Option<Proposal>>;

    /// Move every pending proposal whose expiry has passed to `expired`, and
    /// report how many moved.
    ///
    /// Tidying, not enforcement. See the module docs.
    async fn expire_due(&self, now: DateTime<Utc>) -> Result<u64>;
}
