//! Yielding the engine the moment somebody comes back.
//!
//! Six background passes each hand-rolled this: memory consolidation,
//! conversation re-titling, the rolling-summary refresh, the personal-context
//! index sweep, the compaction pass, and the proactive reviewer. They agreed on
//! the intent and disagreed on nearly everything else — three read both
//! activity sources and three only the in-process clock; two compared against a
//! baseline while two asked whether activity was merely RECENT; tick periods
//! ranged from 500 ms to 5 s.
//!
//! The consequence was not academic. A voice turn arrives in a separate process
//! and only reaches the in-process clock once its turn is persisted, so the
//! passes reading one source could not be interrupted by it — and those were
//! exactly the passes the voice turn then had to queue behind on the serial
//! engine.
//!
//! # Baseline, not recency
//!
//! The predicate is "has activity happened since this pass was admitted", never
//! "was there activity recently". Recency was already ruled on by the gate that
//! let the pass start; asking again mid-pass makes the watcher overrule the
//! gate. That is not hypothetical — the index sweep's watcher used a recency
//! test and cancelled passes the gate had deliberately admitted, so pressing
//! Reindex within fifteen minutes of any turn cleared the index and refilled a
//! fraction of it (fixed in c0ae56c3, and this module is that fix generalised).
//!
//! # Two sources, OR, sampled differently
//!
//! The in-process clock is a lock read, so it is sampled every tick. The
//! database is a query, so it is sampled every Nth — hammering SQLite for the
//! length of a pass costs more than the pass saves. A read failure is `None`,
//! and `None` is *no evidence*, never *activity*: a transient error must not
//! abort a run that was going fine.
//!
//! The database source is filtered through [`human_activity`], never raw
//! sessions: the pond mints its own `sched-` sessions for background work, and
//! a pass that counted those would cancel itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::shared::domain::session_activity::human_activity;
use crate::user_data::ports::session_storage::SessionStorage;

/// The activity reading a pass was admitted against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityBaseline {
    /// `last_user_activity` as it stood when this pass was admitted.
    pub in_process_at: Instant,
    /// Newest human `sessions.updated_at` as it stood then. `None` when the
    /// pond held no human conversation yet.
    pub db_activity: Option<DateTime<Utc>>,
}

/// Whether activity has happened since `baseline`.
///
/// Pure, so the decision can be tested without a clock or a database.
pub fn resumed_since(
    baseline: ActivityBaseline,
    in_process_now: Instant,
    db_now: Option<DateTime<Utc>>,
) -> bool {
    let in_process = in_process_now > baseline.in_process_at;
    let in_db = match db_now {
        // No baseline row means the pond held no human conversation when the
        // pass began, so any human row now is somebody arriving.
        Some(latest) => baseline.db_activity.is_none_or(|seen| latest > seen),
        // A failed or empty read is no evidence, not activity.
        None => false,
    };
    in_process || in_db
}

/// Newest human session activity, or `None` when there is none to read.
///
/// A storage error reads as `None` for the reason above: no evidence.
pub async fn newest_human_activity(storage: &dyn SessionStorage) -> Option<DateTime<Utc>> {
    let sessions = storage.list_sessions().await.ok()?;
    human_activity(&sessions).newest_activity
}

/// Read both sources now, for a caller about to admit a pass.
pub async fn baseline_now(
    in_process: &Arc<RwLock<Instant>>,
    storage: &dyn SessionStorage,
) -> ActivityBaseline {
    ActivityBaseline {
        in_process_at: *in_process.read().await,
        db_activity: newest_human_activity(storage).await,
    }
}

/// Which sources a watcher consults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivitySources {
    /// The in-process clock only.
    ///
    /// A legitimate configuration, not an unfinished one: `sessions.updated_at`
    /// lags a turn by however long that turn takes to persist, so a pass that
    /// must react the instant somebody starts typing reads the clock alone.
    InProcessOnly,
    /// Both, with the database sampled every `db_every_n_ticks` ticks.
    InProcessAndDatabase { db_every_n_ticks: u32 },
}

/// How one watcher runs.
#[derive(Debug, Clone, Copy)]
pub struct WatchConfig {
    pub tick: Duration,
    pub sources: ActivitySources,
    /// Named in the one INFO line a cancellation emits. A chore that yields
    /// silently is indistinguishable from one that is broken.
    pub what_is_cancelled: &'static str,
}

impl WatchConfig {
    /// The shape four of the six sites already used: 500 ms in-process,
    /// database every fourth tick.
    pub fn chore(what_is_cancelled: &'static str) -> Self {
        Self {
            tick: Duration::from_millis(500),
            sources: ActivitySources::InProcessAndDatabase {
                db_every_n_ticks: 4,
            },
            what_is_cancelled,
        }
    }

    /// In-process only, at a caller-chosen tick.
    pub fn in_process_only(tick: Duration, what_is_cancelled: &'static str) -> Self {
        Self {
            tick,
            sources: ActivitySources::InProcessOnly,
            what_is_cancelled,
        }
    }
}

/// A running watcher. Dropping it stops the task.
///
/// The guard shape is deliberate and copied from `LaneSlot`: six call sites
/// each aborted their watcher by hand, some of them twice per loop iteration,
/// and any future early return would have leaked one. A guard cannot be
/// forgotten by a `break`, a `?`, or a panic.
pub struct ActivityWatcher {
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for ActivityWatcher {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Cancel `pass` as soon as activity appears after `baseline`.
///
/// The baseline is passed in rather than read here on purpose: a caller takes
/// it at the moment its gate admitted the pass, and re-reading it inside would
/// move it past whatever happened while the lane slot was being acquired.
pub fn watch_for_return(
    baseline: ActivityBaseline,
    in_process: Arc<RwLock<Instant>>,
    storage: Arc<dyn SessionStorage>,
    pass: CancellationToken,
    config: WatchConfig,
) -> ActivityWatcher {
    let handle = tokio::spawn(async move {
        let mut tick: u32 = 0;
        loop {
            tokio::time::sleep(config.tick).await;
            if pass.is_cancelled() {
                return;
            }
            tick = tick.wrapping_add(1);

            let db_now = match config.sources {
                ActivitySources::InProcessOnly => None,
                ActivitySources::InProcessAndDatabase { db_every_n_ticks } => {
                    let due = db_every_n_ticks > 0 && tick % db_every_n_ticks == 0;
                    if due {
                        newest_human_activity(storage.as_ref()).await
                    } else {
                        None
                    }
                }
            };

            if resumed_since(baseline, *in_process.read().await, db_now) {
                tracing::info!(
                    what = config.what_is_cancelled,
                    "somebody is back — yielding the engine"
                );
                pass.cancel();
                return;
            }
        }
    });
    ActivityWatcher { handle }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs as i64, 0).unwrap()
    }

    fn baseline(db: Option<DateTime<Utc>>) -> ActivityBaseline {
        ActivityBaseline {
            in_process_at: Instant::now(),
            db_activity: db,
        }
    }

    #[test]
    fn nothing_new_is_not_a_reason_to_stop() {
        let b = baseline(Some(at(10)));
        assert!(!resumed_since(b, b.in_process_at, Some(at(10))));
    }

    #[test]
    fn a_newer_in_process_reading_stops_the_pass() {
        let b = baseline(Some(at(10)));
        let later = b.in_process_at + Duration::from_millis(1);
        assert!(resumed_since(b, later, Some(at(10))));
    }

    /// The reason the single-source copies were wrong: a voice turn arrives in
    /// another process and reaches only the database.
    #[test]
    fn a_turn_visible_only_in_the_database_stops_the_pass() {
        let b = baseline(Some(at(10)));
        assert!(resumed_since(b, b.in_process_at, Some(at(11))));
    }

    /// A transient read failure must not abort a run that was going fine.
    #[test]
    fn an_unreadable_database_is_no_evidence_rather_than_activity() {
        let b = baseline(Some(at(10)));
        assert!(!resumed_since(b, b.in_process_at, None));
    }

    /// A pond that held no human conversation when the pass began, and holds
    /// one now, has somebody in it.
    #[test]
    fn a_first_ever_conversation_counts_against_an_empty_baseline() {
        let b = baseline(None);
        assert!(resumed_since(b, b.in_process_at, Some(at(1))));
        assert!(!resumed_since(b, b.in_process_at, None));
    }

    /// Older rows are not activity — clocks and replicas move backwards.
    #[test]
    fn an_older_database_row_does_not_stop_the_pass() {
        let b = baseline(Some(at(10)));
        assert!(!resumed_since(b, b.in_process_at, Some(at(9))));
    }

    /// The distinction this module exists to enforce: the gate already ruled on
    /// recency when it admitted the pass, so only what happens AFTER counts.
    #[test]
    fn recent_activity_before_the_baseline_is_not_a_return() {
        // A pass admitted now, against a database row from ten seconds ago —
        // recent by any measure, and not a reason to stop.
        let b = baseline(Some(at(10)));
        assert!(!resumed_since(b, b.in_process_at, Some(at(10))));
    }

    #[tokio::test]
    async fn the_watcher_cancels_when_the_in_process_clock_moves() {
        let clock = Arc::new(RwLock::new(Instant::now()));
        let pass = CancellationToken::new();
        let _watcher = watch_for_return(
            ActivityBaseline {
                in_process_at: *clock.read().await,
                db_activity: None,
            },
            clock.clone(),
            Arc::new(crate::user_data::mocks::mock_session::InMemorySessionStorage::new()),
            pass.clone(),
            WatchConfig::in_process_only(Duration::from_millis(10), "the test pass"),
        );

        assert!(!pass.is_cancelled());
        *clock.write().await = Instant::now() + Duration::from_millis(1);
        tokio::time::timeout(Duration::from_secs(2), pass.cancelled())
            .await
            .expect("the watcher never noticed the clock move");
    }

    /// Dropping the guard must stop the task, so no early return can leak one.
    #[tokio::test]
    async fn dropping_the_guard_stops_the_watcher() {
        let clock = Arc::new(RwLock::new(Instant::now()));
        let pass = CancellationToken::new();
        let watcher = watch_for_return(
            ActivityBaseline {
                in_process_at: *clock.read().await,
                db_activity: None,
            },
            clock.clone(),
            Arc::new(crate::user_data::mocks::mock_session::InMemorySessionStorage::new()),
            pass.clone(),
            WatchConfig::in_process_only(Duration::from_millis(10), "the test pass"),
        );
        drop(watcher);

        // Activity after the drop must NOT cancel: the task is gone.
        *clock.write().await = Instant::now() + Duration::from_millis(1);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            !pass.is_cancelled(),
            "the dropped watcher was still running"
        );
    }
}
