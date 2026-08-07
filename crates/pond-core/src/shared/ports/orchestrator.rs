//! Driven port: run a child agent — PAI-6 P1.
//!
//! `pond-core` owns *what may run and under what limits*
//! ([`crate::shared::domain::orchestration`]); an adapter owns *how a child
//! agent is executed*. This is the seam between them, and it is deliberately
//! the whole of P1's contact with the outside: there is no implementation in
//! this commit. PAI-3 P1 and PAI-1 P3 both landed domain-first for the same
//! reason — the wiring is smaller and more reviewable when the shape is already
//! settled.
//!
//! # No default method bodies
//!
//! Every method is required. A defaulted trait method is one of this
//! programme's recorded vacuity shapes: deleting a real override leaves the
//! tree green while the feature silently stops working, because the default
//! answers plausibly. An orchestrator that cannot cancel is not an
//! orchestrator, so `cancel` has no body here to fall back to.

use crate::shared::domain::orchestration::{TaskRun, TaskSpec};
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: child agent execution.
#[async_trait]
pub trait Orchestrator: Send + Sync {
    /// Start the run described by `spec`.
    ///
    /// The spec already carries the parent's session id and the child's derived
    /// authority, so there is no separate `parent` argument. The design sketch
    /// had `spawn(spec, parent: &SessionRef)`; passing the parent twice would
    /// let the two disagree, and the one nearer the engine would win — which is
    /// exactly how a scope gets widened by accident. One source of truth.
    ///
    /// Returns as soon as the run has an identity. A synchronous
    /// implementation may return an already-terminal [`TaskRun`]; a background
    /// one returns [`TaskStatus::Running`] and the caller polls.
    ///
    /// [`TaskStatus::Running`]: crate::shared::domain::orchestration::TaskStatus::Running
    async fn spawn(&self, spec: TaskSpec) -> Result<TaskRun>;

    /// Current state of a run. `Ok(None)` when nothing by that id is known —
    /// distinct from an error, because an unknown id is an ordinary answer to a
    /// model that hallucinated one.
    async fn poll(&self, task_id: &str) -> Result<Option<TaskRun>>;

    /// Stop a run. Idempotent: cancelling a finished or unknown run is `Ok`.
    ///
    /// An implementation must record the outcome as
    /// [`TaskStatus::Cancelled`](crate::shared::domain::orchestration::TaskStatus::Cancelled)
    /// rather than letting it look like a completion. Goose's own child loop
    /// breaks out and returns `Ok(partial_text)` when its token trips, so
    /// cancellation is indistinguishable from success at the return type unless
    /// the adapter re-checks the token after the await.
    async fn cancel(&self, task_id: &str) -> Result<()>;

    /// Every run belonging to one parent session, terminal ones included.
    async fn list(&self, parent_session_id: &str) -> Result<Vec<TaskRun>>;

    /// Cancel every child of a parent session, returning how many were stopped.
    ///
    /// PAI-6 invariant 5 is "every spawn is cancellable, and **cancelling a
    /// parent cancels its children**". The second half needs its own method
    /// because nothing upstream is holding the children's ids: the parent turn
    /// owns a cancellation token created inline in the adapter's stream
    /// closure, stored in no map and exposed by no accessor. Whatever ends a
    /// parent — a cancelled turn, a deleted session, a shutdown — calls this.
    async fn cancel_children_of(&self, parent_session_id: &str) -> Result<usize>;
}
