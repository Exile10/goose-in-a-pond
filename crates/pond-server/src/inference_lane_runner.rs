//! The runtime half of the inference lane.
//!
//! [`pond_core::user_data::services::inference_lane`] decides *which* background
//! job may spend inference next. This owns the part that cannot be a pure
//! function: the registry of what each job currently wants, and the single slot
//! that makes "one at a time" true rather than merely intended.
//!
//! # Why a coordinator rather than one big loop
//!
//! Each background job keeps its own poll cadence, its own body and its own
//! cancellation watcher — those differ enough (60s vs 5min, one sweep vs one
//! pipeline) that merging them would be a rewrite with no gain. What they must
//! NOT keep is a private answer to "may I run now?", because that answer has to
//! account for jobs this loop has never heard of. So each loop asks the lane
//! instead, and the lane is the only thing that says yes.
//!
//! # The two guarantees
//!
//! 1. **At most one job runs at a time**, because [`LaneSlot`] is a `Mutex` guard
//!    and a job holds it for the whole of its run. This is what replaces the
//!    pairwise "stand down while consolidation is mid-run" check that titling
//!    used to carry — a check that only ever ran in one direction.
//! 2. **No job starves**, because the decision is least-recently-run rather than
//!    a fixed priority. See `inference_lane::select_next`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pond_core::user_data::services::inference_lane::{
    self, JobState, LaneDecision, LaneInputs, LaneJob,
};

/// Proof that the holder owns the inference slot.
///
/// Dropping it releases the slot, so a job that returns early — or panics —
/// cannot wedge the lane. That is the reason this is a guard and not a boolean:
/// every early return in a job body is a path somebody would have had to
/// remember to write a release on.
pub struct LaneSlot<'a> {
    _guard: tokio::sync::MutexGuard<'a, ()>,
    lane: &'a InferenceLane,
    job: LaneJob,
}

/// Spending the interval budget is tied to the guard's lifetime, exactly like
/// releasing the slot, and for the same reason.
///
/// This used to be an explicit `finish()` the job body had to call, which made
/// the release automatic and the spend manual — and every early return was then
/// a path somebody had to remember. Two of them were missed immediately: the
/// titling loop returns early when no LLM provider is configured and when the
/// session list read fails, both AFTER taking the slot.
///
/// The consequence was not a missed pass, it was permanent starvation. A job
/// that never records a run keeps `since_last_run: None`, which
/// `select_next` treats as infinitely starved — so it wins every tie against
/// every job that HAS run, on every tick, forever. On any pond without a
/// provider configured, titling would have taken the slot and silently locked
/// consolidation out for the life of the process: the precise failure the lane
/// was built to make impossible.
///
/// An attempt spends the budget whatever the outcome — cancelled, or having
/// found nothing to do — because retrying a fruitless expensive pass on every
/// tick is the churn the interval floor exists to prevent.
impl Drop for LaneSlot<'_> {
    fn drop(&mut self) {
        // A std mutex, not tokio's, so this is lockable from `drop`. Safe
        // against the lock order in `acquire`, which releases `last_run` before
        // it ever reaches for the slot.
        match self.lane.last_run.lock() {
            Ok(mut last_run) => {
                last_run.insert(self.job, Instant::now());
            }
            // A poisoned lock means another thread panicked mid-insert. Recover
            // the map rather than panicking again inside a drop, which would
            // abort the process.
            Err(poisoned) => {
                poisoned.into_inner().insert(self.job, Instant::now());
            }
        }
    }
}

/// What a job currently wants, refreshed by that job on every one of its ticks.
///
/// Held by the lane so that a decision made on one job's tick can account for
/// every other job's cadence, including jobs whose own poll is minutes away.
#[derive(Debug, Clone, Copy)]
struct Registration {
    enabled: bool,
    interval_floor: Duration,
    /// This job's own quiet requirement — see `JobState::idle_threshold`.
    idle_threshold: Duration,
}

/// The shared inference slot and the registry of what wants it.
pub struct InferenceLane {
    slot: tokio::sync::Mutex<()>,
    /// Std rather than tokio so [`LaneSlot`]'s `Drop` can record a run. Only
    /// ever held for a map insert or read, never across an await.
    last_run: std::sync::Mutex<HashMap<LaneJob, Instant>>,
    registry: tokio::sync::Mutex<HashMap<LaneJob, Registration>>,
}

impl InferenceLane {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            slot: tokio::sync::Mutex::new(()),
            last_run: std::sync::Mutex::new(HashMap::new()),
            registry: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Ask for the inference slot on behalf of `job`.
    ///
    /// `Some` means this job won the tick and now holds the slot until the
    /// guard is dropped. `None` means either the shared
    /// gate refused every job (the household is mid-conversation, or nothing has
    /// happened since boot), another job was more starved, or another job is
    /// mid-run right now.
    ///
    /// The last of those is why this uses `try_lock` rather than waiting: a job
    /// that queued for the slot would run *after* the conditions that qualified
    /// it had passed — the household could be back, and the whole point of the
    /// idle gate is that background work yields to people. Missing a pass is
    /// correct; the next tick is a minute away.
    pub async fn acquire(
        &self,
        job: LaneJob,
        enabled: bool,
        interval_floor: Duration,
        saw_activity_since_start: bool,
        idle_for: Duration,
        idle_threshold: Duration,
    ) -> Option<LaneSlot<'_>> {
        {
            let mut registry = self.registry.lock().await;
            registry.insert(
                job,
                Registration {
                    enabled,
                    interval_floor,
                    idle_threshold,
                },
            );
        }

        let decision = {
            let registry = self.registry.lock().await;
            let last_run = self
                .last_run
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let mut states: Vec<JobState> = registry
                .iter()
                .map(|(&j, reg)| JobState {
                    job: j,
                    enabled: reg.enabled,
                    since_last_run: last_run.get(&j).map(|t| now.duration_since(*t)),
                    interval_floor: reg.interval_floor,
                    idle_threshold: reg.idle_threshold,
                })
                .collect();

            // The registry is a HashMap, so this vec arrives in whatever order
            // hashing produced. `select_next` breaks ties by position, so
            // handing it an arbitrary order would make two equally-starved jobs
            // resolve differently between runs — a coin flip that presents as a
            // job which "sometimes doesn't run". Sorting restores the documented
            // tie-break; `LaneJob: Ord` is declaration order.
            states.sort_unstable_by_key(|s| s.job);

            inference_lane::select_next(LaneInputs {
                saw_activity_since_start,
                idle_for,
                jobs: &states,
            })
        };

        match decision {
            LaneDecision::Run(winner) if winner == job => {}
            LaneDecision::Run(winner) => {
                tracing::trace!(
                    asked = job.as_str(),
                    running = winner.as_str(),
                    "inference lane: another job is more starved"
                );
                return None;
            }
            LaneDecision::Idle(reason) => {
                tracing::trace!(
                    asked = job.as_str(),
                    reason = reason.as_str(),
                    "inference lane: no job may run"
                );
                return None;
            }
        }

        // Won the decision — now take the slot, or stand down. See the doc
        // comment for why this does not wait.
        match self.slot.try_lock() {
            Ok(guard) => {
                tracing::debug!(job = job.as_str(), "inference lane: slot acquired");
                Some(LaneSlot {
                    _guard: guard,
                    lane: self,
                    job,
                })
            }
            Err(_) => {
                tracing::trace!(
                    asked = job.as_str(),
                    "inference lane: slot busy, standing down until the next tick"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE_THRESHOLD: Duration = Duration::from_secs(900);
    const LONG_IDLE: Duration = Duration::from_secs(3600);

    async fn ask(lane: &InferenceLane, job: LaneJob) -> Option<LaneSlot<'_>> {
        lane.acquire(job, true, Duration::ZERO, true, LONG_IDLE, IDLE_THRESHOLD)
            .await
    }

    #[tokio::test]
    async fn a_second_job_cannot_take_a_held_slot() {
        // The guarantee the pairwise stand-down checks were reaching for, now
        // enforced by the type rather than by each job remembering to look.
        let lane = InferenceLane::new();
        let held = ask(&lane, LaneJob::Consolidation)
            .await
            .expect("first caller should win an empty lane");

        assert!(
            ask(&lane, LaneJob::Titling).await.is_none(),
            "titling took the slot while consolidation held it"
        );

        drop(held);
    }

    #[tokio::test]
    async fn releasing_the_slot_lets_the_next_job_in() {
        let lane = InferenceLane::new();
        drop(
            ask(&lane, LaneJob::Consolidation)
                .await
                .expect("first caller wins"),
        );

        assert!(
            ask(&lane, LaneJob::Titling).await.is_some(),
            "the slot was not released"
        );
    }

    #[tokio::test]
    async fn a_dropped_slot_does_not_wedge_the_lane() {
        // An early return in a job body drops the guard without calling finish.
        // The slot must come back even though the interval budget was not spent.
        //
        // Re-asks with the SAME job on purpose. Asking with a different one
        // would conflate two things: a job can also be refused because it lost
        // the decision, and with both jobs never-run they tie and the earlier
        // one wins — so a passing assertion there would say nothing about the
        // slot. The same job cannot lose to itself, which leaves the slot as
        // the only thing under test.
        let lane = InferenceLane::new();
        {
            let _slot = ask(&lane, LaneJob::Consolidation).await.expect("wins");
        }
        assert!(
            ask(&lane, LaneJob::Consolidation).await.is_some(),
            "dropping a slot without finish() left the lane wedged"
        );
    }

    #[tokio::test]
    async fn a_job_that_bails_early_still_spends_its_budget() {
        // The starvation bug, in the shape it actually occurred: titling takes
        // the slot, finds no LLM provider configured, and returns early without
        // any explicit bookkeeping call.
        //
        // If that leaves it with `since_last_run: None` it counts as infinitely
        // starved and wins every subsequent tie forever, locking consolidation
        // out for the life of the process. The assertion is therefore about the
        // NEXT decision, not about any state the lane exposes.
        // The bailing job must be the EARLIER-declared one. Ties break by
        // declaration order, so if the later job bailed, the earlier one would
        // win the next tick regardless of whether the budget was spent, and the
        // assertion would hold with the bug fully present. The first version of
        // this test made exactly that mistake and passed against the bug.
        let lane = InferenceLane::new();
        {
            let _slot = ask(&lane, LaneJob::Consolidation)
                .await
                .expect("wins an empty lane");
            // ...body bails here. No bookkeeping call, just a drop.
        }

        assert!(
            ask(&lane, LaneJob::Titling).await.is_some(),
            "consolidation bailed early and kept its never-run status, so it \
             out-starves every job that HAS run and wins every tie forever — \
             titling can no longer take the slot at all"
        );
    }

    #[tokio::test]
    async fn the_shared_gate_refuses_everyone_mid_conversation() {
        let lane = InferenceLane::new();
        let slot = lane
            .acquire(
                LaneJob::Consolidation,
                true,
                Duration::ZERO,
                true,
                Duration::from_secs(5), // user active 5s ago
                IDLE_THRESHOLD,
            )
            .await;
        assert!(slot.is_none());
    }

    #[tokio::test]
    async fn a_job_registers_even_when_it_loses_so_others_can_see_it() {
        // Titling asks first and loses nothing (empty lane), but the point is
        // that its registration persists: consolidation's later tick must be
        // decided against a lane that knows titling exists.
        let lane = InferenceLane::new();
        drop(ask(&lane, LaneJob::Titling).await.expect("wins"));

        let registry = lane.registry.lock().await;
        assert!(registry.contains_key(&LaneJob::Titling));
    }
}
