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
//! to start. PAI-7 P4's reviewer now runs [`expire_due`](ProposalRepository::expire_due)
//! once a tick, and that changed nothing about this rule — which is the point of
//! having written it this way. Passing the clock in makes the filter unskippable
//! and makes it testable without waiting; the sweep keeps the table tidy and
//! gives a row the terminal status the feedback loop reads.

use crate::user_data::domain::proposal::{Proposal, ProposalDecision};
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

    /// How many proposals were **made** to one member since `since`, whatever
    /// became of them.
    ///
    /// This is what `MAX_PROPOSALS_PER_DAY` has to be counted against, and it
    /// is deliberately the one read here that does **not** filter on live or
    /// pending. The cap is a limit on how often the pond interrupts somebody,
    /// and a member who has read and dismissed three suggestions has been
    /// interrupted three times — counting only what is still pending would let
    /// a decisive user be pestered without limit while a passive one is
    /// protected, which is precisely backwards.
    ///
    /// It also makes the cap survive a restart, which an in-process counter
    /// cannot: PAI-7's verification section asks for exactly that.
    async fn count_made_since(&self, profile_id: &str, since: DateTime<Utc>) -> Result<usize>;

    /// What one member has already decided about proposals made since `since`
    /// — PAI-7 P7's read, and the only one the feedback loop needs.
    ///
    /// Pending rows are excluded, because [`ProposalDecision::recorded`] refuses
    /// [`DraftStatus::Pending`]: a ledger of decisions containing an undecided
    /// row is a fact nobody stated. Expiries ARE included and are not silence
    /// dressed as refusal — `silences_a_repeat` disposes of that distinction,
    /// which is the domain's job and not this query's.
    ///
    /// [`ProposalDecision::recorded`]: crate::user_data::domain::proposal::ProposalDecision::recorded
    /// [`DraftStatus::Pending`]: crate::user_data::domain::draft::DraftStatus::Pending
    async fn decisions_since(
        &self,
        profile_id: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<ProposalDecision>>;
}
