//! `CronSchedulerAdapter` — implements `SchedulerPort` via `tokio-cron-scheduler`.
//!
//! Task state (id, label, cron, kind, timezone, paused) is persisted to a JSON
//! file so tasks survive process restarts.  The scheduler job map is rebuilt
//! from the persisted state on startup.
//!
//! Execution history is stored in a separate JSON file via [`JsonRunHistory`].

use crate::run_history::JsonRunHistory;
use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::user_data::domain::schedule::{RunStatus, Schedule, ScheduleRun, TaskKind};
use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
use pond_core::user_data::ports::scheduler::{
    CreateScheduleRequest, SchedulerPort, UpdateScheduleRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_cron_scheduler::{Job, JobScheduler};

// ── Persisted record ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedTask {
    id: String,
    label: String,
    cron: String,
    /// IANA timezone.  Defaults to "UTC" for legacy tasks.
    #[serde(default = "default_timezone")]
    timezone: String,
    /// Has this task's zone been through the one-time legacy backfill?
    ///
    /// Absent (`false`) on any file written before schedules honoured their
    /// zone. Back then the UI defaulted the picker to "UTC" and the scheduler
    /// ignored the value anyway, so a stored "UTC" recorded no decision — it
    /// was just what the form submitted. The backfill reads those as "meant
    /// local" and rewrites them once.
    ///
    /// The flag is what makes that a MIGRATION rather than a standing
    /// override: without it, a household that deliberately chooses UTC would
    /// have the choice silently undone on every restart.
    #[serde(default)]
    tz_migrated: bool,
    /// What the task does on each fire.
    /// `None` for legacy tasks — migrated from `payload` during rehydration.
    #[serde(default)]
    kind: Option<TaskKind>,
    /// Legacy field — kept for backward-compat deserialization.
    #[serde(default)]
    payload: Option<serde_json::Value>,
    paused: bool,
    #[serde(default)]
    created_at: Option<chrono::DateTime<Utc>>,
    /// When this task last FIRED (not when it finished). Set for every kind;
    /// what [`durable_fire_stamp`] decides is which kinds a fire is worth a
    /// write FOR, so a sensor rule's cooldown survives a restart and a cron
    /// task's stamp reaches disk on the next save somebody else asks for.
    /// `None` on a file written before PAI-7 P8, which reads as "never fired":
    /// the first event after an upgrade fires once, and the stamp exists from
    /// then on.
    #[serde(default)]
    last_run: Option<chrono::DateTime<Utc>>,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

// ── In-memory state ───────────────────────────────────────────────────────────

struct TaskEntry {
    persisted: PersistedTask,
    job_id: uuid::Uuid,
    currently_running: bool,
    last_run: Option<chrono::DateTime<Utc>>,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct CronSchedulerAdapter {
    scheduler: JobScheduler,
    tasks: Arc<Mutex<HashMap<String, TaskEntry>>>,
    persist: Arc<SnapshotWriter>,
    executor: Arc<dyn ScheduleExecutor>,
    run_history: Arc<JsonRunHistory>,
    /// Optional broadcast sender for schedule result events (SSE delivery).
    result_tx: Option<
        tokio::sync::broadcast::Sender<pond_core::user_data::domain::schedule::ScheduleResultEvent>,
    >,
    /// The household's configured zone, used only by the legacy backfill in
    /// [`Self::rehydrate`].
    ///
    /// Injected rather than read through a port: this is an infra adapter and
    /// `pond-server` already has the settings loaded long before it builds the
    /// scheduler, so taking a `SettingsRepository` dependency here would buy
    /// nothing and point the arrow the wrong way.
    household_timezone: String,
}

impl CronSchedulerAdapter {
    /// Create (or rehydrate) the scheduler.
    ///
    /// `persist_path`  — path to the JSON file used to persist task definitions.
    /// `runs_path`     — path to the JSON file for execution history.
    /// `executor`      — called on each job fire.
    pub async fn new(
        persist_path: PathBuf,
        runs_path: PathBuf,
        executor: Arc<dyn ScheduleExecutor>,
    ) -> Result<Self> {
        Self::with_options(persist_path, runs_path, executor, None, 50, "UTC").await
    }

    /// Create with all options.
    ///
    /// `household_timezone` — the zone from Settings, used only to backfill
    /// schedules stored before the scheduler honoured zones at all.
    pub async fn with_options(
        persist_path: PathBuf,
        runs_path: PathBuf,
        executor: Arc<dyn ScheduleExecutor>,
        result_tx: Option<
            tokio::sync::broadcast::Sender<
                pond_core::user_data::domain::schedule::ScheduleResultEvent,
            >,
        >,
        max_runs_per_task: u32,
        household_timezone: &str,
    ) -> Result<Self> {
        let scheduler = JobScheduler::new().await?;
        let tasks: Arc<Mutex<HashMap<String, TaskEntry>>> = Arc::new(Mutex::new(HashMap::new()));
        let run_history = Arc::new(JsonRunHistory::new(runs_path, max_runs_per_task).await?);

        let adapter = Self {
            scheduler,
            tasks,
            persist: Arc::new(SnapshotWriter::new(persist_path)),
            executor,
            run_history,
            result_tx,
            household_timezone: household_timezone.to_string(),
        };

        // Rehydrate persisted tasks
        adapter.rehydrate().await?;

        adapter.scheduler.start().await?;

        Ok(adapter)
    }

    // ── Persistence helpers ───────────────────────────────────────────────────

    async fn save(&self) -> Result<()> {
        let guard = self.tasks.lock().await;
        // The ticket is taken here, under the tasks lock, so it orders this
        // snapshot by when its CONTENTS were read. See [`SnapshotWriter`].
        let seq = self.persist.ticket();
        let records: Vec<PersistedTask> = guard.values().map(|e| e.persisted.clone()).collect();
        drop(guard);

        self.persist.publish(seq, &records).await
    }

    /// Record that `id` just FIRED — in memory always, and on disk for the
    /// kinds whose debounce reads the stamp back after a restart.
    ///
    /// Stamped at the START of the run, not at the end, and that is the point:
    /// a pond that crashes or is killed mid-run has still fired, and a stamp
    /// written only on completion is exactly the one a crash loop never gets to
    /// write. `last_run` therefore means "last fired", which is also what the
    /// scheduler UI is asking.
    ///
    /// Takes the pieces rather than `&self` because both fire paths run inside
    /// a spawned task that has outlived the borrow.
    async fn stamp_fire(
        tasks: &Arc<Mutex<HashMap<String, TaskEntry>>>,
        persist: &Arc<SnapshotWriter>,
        id: &str,
    ) {
        let now = Utc::now();
        let (seq, records): (u64, Vec<PersistedTask>) = {
            let mut guard = tasks.lock().await;
            let Some(entry) = guard.get_mut(id) else {
                return;
            };
            // In memory for every kind, because "last run" is what the
            // schedules UI shows and a cron task has one too. What
            // `durable_fire_stamp` gates below is the WRITE.
            entry.last_run = Some(now);
            entry.persisted.last_run = Some(now);
            if !durable_fire_stamp(&Self::resolve_kind(entry)) {
                return;
            }
            // Ticketed under the tasks lock, same as `save`. A fire stamp and
            // an API edit are the two writers that race in production, and
            // this is what stops the slower one publishing the older state.
            (
                persist.ticket(),
                guard.values().map(|e| e.persisted.clone()).collect(),
            )
        };

        if let Err(e) = persist.publish(seq, &records).await {
            // A lost stamp re-fires the rule after the next restart, which is
            // the defect this exists to fix — so it is a warning, not a debug.
            tracing::warn!(task = %id, error = %e, "could not persist fire stamp");
        }
    }

    async fn rehydrate(&self) -> Result<()> {
        let path = self.persist.path();
        if !path.exists() {
            return Ok(());
        }

        let json = tokio::fs::read_to_string(path).await?;
        let records: Vec<PersistedTask> = match serde_json::from_str(&json) {
            Ok(records) => records,
            Err(e) => {
                // A file that does not parse is not a file to keep reading on
                // every boot. `pond-server` turns this constructor's Err into
                // `scheduler = None`, so returning one here means every
                // schedule endpoint answers 503 for the life of the install
                // and the next boot does the same. Start empty instead -- but
                // never by deleting the only copy of the household's
                // automations, so the unreadable file is moved aside and
                // named in the log.
                let kept = quarantine_unreadable(path).await;
                let kept = kept
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "NOT SAVED".to_string());
                tracing::error!(
                    path = %path.display(),
                    kept = %kept,
                    error = %e,
                    "schedules.json is unreadable: starting with NO schedules and NO rules"
                );
                return Ok(());
            }
        };

        let mut migrated = false;
        for mut record in records {
            // ── Backward-compatible migration ────────────────────────────
            // Old tasks have `payload` but no `kind`.  Infer from payload.
            if record.kind.is_none() {
                record.kind = Some(migrate_kind_from_payload(&record));
                migrated = true;
            }
            if record.created_at.is_none() {
                record.created_at = Some(Utc::now());
                migrated = true;
            }

            // ── One-time timezone backfill ───────────────────────────────
            // A stored "UTC" written before this migration existed is not a
            // choice, it is the old UI default landing in a field the
            // scheduler then ignored. Read literally after the fix, it would
            // keep every existing schedule firing at the UTC hour — an 08:00
            // briefing in Nairobi would go on arriving at 11:00, which is the
            // complaint that started this. Rewrite it once to the household's
            // zone and mark it, so a later deliberate "UTC" is left alone.
            // The `!= "UTC"` guard is on the MARKER as well as the rewrite, and
            // deliberately so: on a pond whose household zone is still the
            // default there is nothing to migrate TO, and burning the flag
            // there would mean the backfill never runs once a real zone is
            // finally set during onboarding.
            if !record.tz_migrated && self.household_timezone != "UTC" {
                record.tz_migrated = true;
                migrated = true;
                if record.timezone == "UTC" {
                    tracing::info!(
                        task = %record.id,
                        timezone = %self.household_timezone,
                        "migrating a schedule stored without a timezone to the household zone"
                    );
                    record.timezone = self.household_timezone.clone();
                }
            }

            // A paused task and an event-triggered rule (#92) both load with
            // no cron job -- the nil id is what "no job" means for either.
            let job_id = if record.paused {
                uuid::Uuid::nil()
            } else {
                let kind = record.kind.clone().unwrap();
                if kind.is_event_triggered() {
                    uuid::Uuid::nil()
                } else {
                    self.arm_chain(&record.id, &record.cron, &record.timezone)
                        .await?
                }
            };

            // ONE place the persisted fire stamp can be dropped, and that is
            // why this is one insert rather than two. It was written as two,
            // carrying the stamp separately on the paused branch and on the
            // active one, and only the active branch had a test: setting the
            // paused copy to `None` reintroduced PAI-7 P8's whole defect on
            // the pause-restart-resume path with the suite green. See
            // `a_paused_rules_fire_stamp_survives_a_restart`.
            let last_run = record.last_run;
            let mut guard = self.tasks.lock().await;
            guard.insert(
                record.id.clone(),
                TaskEntry {
                    persisted: record,
                    job_id,
                    currently_running: false,
                    last_run,
                },
            );
        }

        // Re-save if any tasks were migrated to the new format.
        if migrated {
            self.save().await?;
        }

        Ok(())
    }

    // ── Internal job management ───────────────────────────────────────────────

    /// Arm the next occurrence of `task_id` and return the job id holding it.
    ///
    /// See [`arm_next`] for why this is a one-shot chain rather than a
    /// recurring cron job.
    async fn arm_chain(&self, task_id: &str, cron: &str, timezone: &str) -> Result<uuid::Uuid> {
        let ctx = Arc::new(ChainCtx {
            id: task_id.to_string(),
            executor: self.executor.clone(),
            tasks: self.tasks.clone(),
            run_history: self.run_history.clone(),
            result_tx: self.result_tx.clone(),
            persist: self.persist.clone(),
        });
        arm_next(
            ctx,
            self.scheduler.clone(),
            cron.to_string(),
            parse_tz(timezone),
        )
        .await
    }

    /// Resolve the `TaskKind` for a task entry.
    fn resolve_kind(entry: &TaskEntry) -> TaskKind {
        entry
            .persisted
            .kind
            .clone()
            .unwrap_or_else(|| migrate_kind_from_payload(&entry.persisted))
    }

    /// Convert a `TaskEntry` into a `Schedule` domain object.
    fn to_schedule(entry: &TaskEntry) -> Schedule {
        let next_run = if entry.persisted.paused {
            None
        } else {
            compute_next_run(&entry.persisted.cron, &entry.persisted.timezone)
        };
        Schedule {
            id: entry.persisted.id.clone(),
            label: entry.persisted.label.clone(),
            cron: entry.persisted.cron.clone(),
            timezone: entry.persisted.timezone.clone(),
            kind: Self::resolve_kind(entry),
            paused: entry.persisted.paused,
            currently_running: entry.currently_running,
            last_run: entry.last_run,
            next_run,
            created_at: entry.persisted.created_at.unwrap_or_else(Utc::now),
        }
    }
}

// ── The one-shot chain ────────────────────────────────────────────────────────

/// Never arm a timer further out than this.
///
/// A schedule's next occurrence can be a month away, and a timer armed that far
/// ahead is a promise about a wall clock nobody has checked since. An NTP step
/// correction (ordinary on a Jetson that boots offline and syncs minutes later)
/// or a tzdata update both invalidate it silently. Capping the wait means the
/// occurrence is recomputed at least this often, so any such correction costs at
/// most one horizon of drift instead of the whole wait.
const REARM_HORIZON: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// How early a wake-up still counts as "this is the occurrence".
///
/// `tokio-cron-scheduler` polls on a sub-second tick and so fires at or shortly
/// AFTER the instant asked for, never meaningfully before. The tolerance exists
/// for clock jitter — and it doubles as the anti-spin bound: a wake-up that is
/// not due necessarily has more than this long left to wait, so the re-arm
/// delay below can never collapse to zero and busy-loop.
const FIRE_TOLERANCE: chrono::TimeDelta = chrono::TimeDelta::seconds(1);

/// Everything a chained fire needs that does not change between occurrences.
///
/// The cron expression, the zone and the kind are deliberately NOT in here:
/// they are re-read from the task map on every re-arm, so an edit that lands
/// while a chain is armed is picked up by the next link instead of being
/// carried stale until a restart.
struct ChainCtx {
    id: String,
    executor: Arc<dyn ScheduleExecutor>,
    tasks: Arc<Mutex<HashMap<String, TaskEntry>>>,
    run_history: Arc<JsonRunHistory>,
    result_tx: Option<
        tokio::sync::broadcast::Sender<pond_core::user_data::domain::schedule::ScheduleResultEvent>,
    >,
    persist: Arc<SnapshotWriter>,
}

/// Arm a one-shot for the next occurrence, which re-arms itself after firing.
///
/// **Why not a recurring cron job.** `Job::new_async` is defined upstream as
/// `new_async_tz(schedule, Utc, run)` — it evaluates the expression in UTC, full
/// stop. That is the whole bug this replaces: hour `8` fired at 08:00 UTC, which
/// is 11:00 in Nairobi. Its timezone-aware sibling `new_async_tz` is not the fix
/// either, because it snapshots a FIXED offset at registration
/// (`offset_from_utc_datetime(...).fix()`), so a zone with DST drifts by an hour
/// at every transition and stays wrong until something re-registers the job.
///
/// Recomputing the occurrence in the zone before each link is armed makes the
/// zone's rules apply to the date they actually govern, which is DST-correct by
/// construction rather than by remembering to refresh.
///
/// Returns a boxed future rather than being a plain `async fn` because this and
/// [`fire_and_rearm`] are mutually recursive — each link arms the next — and the
/// compiler cannot infer `Send` around that cycle. Naming the bound here breaks
/// it; `tokio-cron-scheduler` requires a `Send` future.
fn arm_next(
    ctx: Arc<ChainCtx>,
    sched: JobScheduler,
    cron: String,
    tz: chrono_tz::Tz,
) -> Pin<Box<dyn Future<Output = Result<uuid::Uuid>> + Send>> {
    Box::pin(async move {
        let next = next_occurrence_utc(&cron, tz).ok_or_else(|| {
            anyhow::anyhow!("cron expression '{cron}' has no upcoming occurrence in timezone {tz}")
        })?;

        // Capped so a distant occurrence is re-checked rather than trusted; the
        // wake-up that results is a no-op hop, not a fire. `armed_for` travels
        // with the closure rather than living on the task entry, so it is
        // correct even for the very first link — `create_task` arms before it
        // inserts.
        let wait = (next - Utc::now())
            .to_std()
            .unwrap_or(std::time::Duration::ZERO)
            .min(REARM_HORIZON);

        let job = Job::new_one_shot_async(wait, move |spent, lock| {
            let ctx = ctx.clone();
            Box::pin(async move { fire_and_rearm(ctx, spent, lock, next).await })
        })
        .map_err(|e| anyhow::anyhow!("invalid cron expression '{cron}': {e}"))?;

        Ok(sched.add(job).await?)
    })
}

/// One link of the chain: fire if this really is the occurrence, then arm the
/// next link.
async fn fire_and_rearm(
    ctx: Arc<ChainCtx>,
    spent: uuid::Uuid,
    sched: JobScheduler,
    armed_for: DateTime<Utc>,
) {
    // The chain's terminator. A deleted task is gone from the map and a paused
    // one is flagged, and either way this link must be the last: re-arming a
    // task that no longer exists resurrects a schedule the household deleted.
    let current = {
        let guard = ctx.tasks.lock().await;
        match guard.get(&ctx.id) {
            Some(entry) if !entry.persisted.paused => Some((
                entry.persisted.cron.clone(),
                entry.persisted.timezone.clone(),
                CronSchedulerAdapter::resolve_kind(entry),
            )),
            _ => None,
        }
    };
    let Some((cron, timezone, kind)) = current else {
        let _ = sched.remove(&spent).await;
        return;
    };

    // A capped wait wakes us before the occurrence; that is a hop, not a fire.
    if Utc::now() >= armed_for - FIRE_TOLERANCE {
        fire_once(&ctx, &kind).await;
    }

    // Re-read cron/zone above rather than reusing what this link was armed
    // with, so an edit mid-wait takes effect on the next link.
    match arm_next(ctx.clone(), sched.clone(), cron, parse_tz(&timezone)).await {
        Ok(next_id) => {
            // Adopt the successor only if THIS link is still the one the task
            // is holding. An edit that lands while the link is executing arms
            // its own chain and removes this link — but removal cannot recall a
            // job already running, so without this check both chains would
            // survive and the schedule would fire twice on every occurrence,
            // forever, with no way to see why from the task list.
            let superseded = {
                let mut guard = ctx.tasks.lock().await;
                match guard.get_mut(&ctx.id) {
                    Some(entry) if entry.job_id == spent => {
                        entry.job_id = next_id;
                        false
                    }
                    _ => true,
                }
            };
            if superseded {
                tracing::debug!(
                    task = %ctx.id,
                    "a newer chain took over while this occurrence ran; dropping the successor"
                );
                let _ = sched.remove(&next_id).await;
            }
        }
        Err(e) => {
            // The chain stops here, so the schedule is dead until a restart or
            // an edit re-arms it. That is worth an error, not a warning.
            tracing::error!(
                task = %ctx.id,
                error = %e,
                "could not arm the next occurrence; this schedule will not fire again until it is edited or the pond restarts"
            );
        }
    }

    // Reap the spent link. Without this the job store grows by one dead entry
    // per fire, which on an always-on pond is a leak measured in months.
    let _ = sched.remove(&spent).await;
}

/// Execute one occurrence: stamp, broadcast, run, record, broadcast.
async fn fire_once(ctx: &ChainCtx, kind: &TaskKind) {
    use pond_core::user_data::domain::schedule::ScheduleResultEvent;

    let id = &ctx.id;

    // Get label for the event
    let label = {
        let guard = ctx.tasks.lock().await;
        guard
            .get(id)
            .map(|e| e.persisted.label.clone())
            .unwrap_or_default()
    };

    // Mark running
    {
        let mut guard = ctx.tasks.lock().await;
        if let Some(entry) = guard.get_mut(id) {
            entry.currently_running = true;
        }
    }
    CronSchedulerAdapter::stamp_fire(&ctx.tasks, &ctx.persist, id).await;

    // Record run start
    let run_id = ctx.run_history.record_start(id).await;
    let start = std::time::Instant::now();

    // Broadcast "started" event so clients see progress immediately
    if let Some(tx) = &ctx.result_tx {
        let _ = tx.send(ScheduleResultEvent {
            schedule_id: id.clone(),
            schedule_label: label.clone(),
            run_id: run_id.clone(),
            status: RunStatus::Running,
            result: None,
            error: None,
            duration_ms: None,
        });
    }

    // Execute
    let result = ctx.executor.execute(id, kind).await;
    let duration_ms = start.elapsed().as_millis() as u64;

    // Record run finish + broadcast event
    let (status, result_text, error_text) = match &result {
        Ok(text) => {
            ctx.run_history
                .record_finish(&run_id, RunStatus::Completed, Some(text.clone()), None)
                .await;
            (RunStatus::Completed, Some(text.clone()), None)
        }
        Err(e) => {
            tracing::error!("Scheduled task {id} failed: {e}");
            ctx.run_history
                .record_finish(&run_id, RunStatus::Failed, None, Some(e.to_string()))
                .await;
            (RunStatus::Failed, None, Some(e.to_string()))
        }
    };

    // Broadcast result event (for SSE / desktop notifications)
    if let Some(tx) = &ctx.result_tx {
        let _ = tx.send(ScheduleResultEvent {
            schedule_id: id.clone(),
            schedule_label: label,
            run_id: run_id.clone(),
            status,
            result: result_text,
            error: error_text,
            duration_ms: Some(duration_ms),
        });
    }

    // Mark not-running. `last_run` was stamped at fire time above.
    {
        let mut guard = ctx.tasks.lock().await;
        if let Some(entry) = guard.get_mut(id) {
            entry.currently_running = false;
        }
    }
}

/// Resolve an IANA zone name, falling back to UTC.
///
/// A zone that does not resolve must not take the schedule down with it: the
/// household would rather have a morning briefing at the wrong hour than no
/// morning briefing and a 500. It is a `warn!` and not a `debug!` because the
/// fallback silently reintroduces the exact defect this module was fixed for.
fn parse_tz(name: &str) -> chrono_tz::Tz {
    name.parse().unwrap_or_else(|_| {
        tracing::warn!(
            timezone = %name,
            "unknown IANA timezone on a schedule; falling back to UTC"
        );
        chrono_tz::Tz::UTC
    })
}

/// The next occurrence of `cron_expr` *in `tz`*, as a UTC instant.
///
/// This is the single source of truth for both when a job is armed and the
/// `next_run` the UI promises. They were two computations before, and they
/// agreed with each other only because BOTH ignored the timezone — which is
/// precisely why a schedule firing three hours late looked correct in the UI.
///
/// Evaluating in the zone (rather than applying a fixed offset to a UTC
/// result) is what makes this DST-correct: croner resolves the local wall
/// time against the zone's rules for that date, so "08:00 every day" stays
/// 08:00 across a transition instead of drifting by an hour.
fn next_occurrence_utc(cron_expr: &str, tz: chrono_tz::Tz) -> Option<DateTime<Utc>> {
    next_occurrence_after(cron_expr, tz, Utc::now())
}

/// [`next_occurrence_utc`] from an arbitrary instant.
///
/// Split out so the DST behaviour can be asserted at a chosen date rather than
/// only at whatever "now" the test suite happens to run at — a schedule that is
/// correct in August and wrong in January is exactly the bug worth catching.
fn next_occurrence_after(
    cron_expr: &str,
    tz: chrono_tz::Tz,
    from: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    // tokio-cron-scheduler uses 6-field cron (sec min hour dom month dow), and
    // both 5- and 6-field expressions have to parse. croner 3 makes that the
    // default — `Seconds::Optional` — so the explicit `.with_seconds_optional()`
    // builder that 2.x needed is gone rather than merely renamed.
    //
    // This must stay on the same croner MAJOR as the one inside
    // `tokio-cron-scheduler`. That constraint is now MORE load-bearing than the
    // comment it replaces described: this parser no longer merely predicts the
    // fire time for the UI, it DECIDES it — `arm_next` schedules a one-shot at
    // whatever this returns. A disagreement between the two copies used to show
    // one time and fire at another; today it would simply fire at the wrong one.
    let cron: croner::Cron = cron_expr.parse().ok()?;
    let from_in_zone = from.with_timezone(&tz);
    match cron.find_next_occurrence(&from_in_zone, false) {
        Ok(dt) => Some(dt.with_timezone(&Utc)),
        Err(e) => {
            tracing::debug!(
                cron_expr,
                timezone = %tz,
                error = %e,
                "Failed to compute next occurrence for cron expression"
            );
            None
        }
    }
}

/// Compute the next fire time for a cron expression from now, in `timezone`.
///
/// Returns `None` if the cron expression is invalid or no upcoming occurrence
/// can be found within a reasonable search window.
fn compute_next_run(cron_expr: &str, timezone: &str) -> Option<DateTime<Utc>> {
    next_occurrence_utc(cron_expr, parse_tz(timezone))
}

/// Refuse a task kind the scheduler must not store.
///
/// Only sensor rules have anything to check today — the domain owns what makes
/// one storable ([`SensorTriggerSpec::validate`]) and this is the seam that
/// makes the answer unavoidable rather than per-caller.
fn validate_kind(kind: &TaskKind) -> Result<()> {
    if let TaskKind::SensorTrigger(spec) = kind {
        spec.validate()
            .map_err(|rejected| anyhow::anyhow!("{rejected}"))?;
    }
    Ok(())
}

/// Is this kind's fire worth a WRITE of its own?
///
/// Read the question precisely, because the loose version of it ("which kinds
/// are persisted") is not what this decides, and the commit that added it said
/// otherwise. `stamp_fire` sets `last_run` in memory for EVERY kind — a cron
/// task has a last run and the schedules UI shows it — and the copy it sets
/// includes the persisted record, so any later create, update, delete, pause or
/// resume carries a cron task's stamp to disk on its own `save()`, and
/// rehydration restores it. What is gated here is only whether a FIRE is itself
/// a reason to rewrite the file, which is the part that costs flash.
///
/// Only a sensor rule reads its own last fire back: the rules engine debounces
/// on it, so a lost stamp re-fires the rule the moment the pond comes back, and
/// a pond that crash-loops fires it every time. That read-back is also what
/// bounds the write — a rule cannot be stamped more often than once per
/// `cooldown_secs`, because the cooldown is the thing the stamp enforces.
///
/// A cron task's next fire is computed from its expression rather than from its
/// last fire, so a write per fire buys nothing, and a 6-field expression is
/// allowed to fire every second — which on the Jetson's flash would be a
/// whole-file rewrite per second. A rule with `cooldown_secs == 0` is excluded
/// for the same reason: it debounces nothing, so it would write on every
/// matching event. Riding along on a save somebody else asked for costs neither
/// of them anything, which is why the in-memory assignment is not gated too.
fn durable_fire_stamp(kind: &TaskKind) -> bool {
    matches!(kind, TaskKind::SensorTrigger(spec) if spec.cooldown_secs > 0)
}

/// Publishes `schedules.json`: one writer at a time, in content order, and
/// through a scratch file no other writer can be holding.
///
/// `rename(2)` is atomic; the WRITE that fills the file it renames is not, and
/// filling that file was the step with no exclusion at all. Every writer used
/// one fixed `schedules.json.tmp`, so two of them truncated and wrote the same
/// inode and the rename published the mixture — and a writer still holding that
/// descriptor went on writing into the file another writer's rename had already
/// made the LIVE `schedules.json`, which no later rename undoes. Atomicity of
/// the wrong step buys nothing.
///
/// It is not a rare shape. `rules_engine` calls `run_now` for EVERY rule
/// matching one bus event and `run_now` returns as soon as it has spawned, so
/// one motion event with two rules on it is two concurrent fire stamps; and
/// PAI-7 P8 put every rule cooldown through this file, so it is written on each
/// fire now rather than on each schedule edit. What is at stake is not one
/// cooldown: a `schedules.json` that does not parse is every schedule and every
/// rule in the pond.
///
/// Two properties, and both are guarded:
///
/// * **A scratch path of its own per write** ([`temp_path`]), so no two writers
///   are ever filling one file. This is the one that stops corruption.
/// * **An order taken from the CONTENT rather than from the writer.**
///   `ticket()` is called while its caller still holds the tasks lock, and a
///   snapshot is skipped when a higher ticket is already on disk. Without it
///   the slow writer lands last and publishes the older state — which for two
///   fire stamps means losing the newest one, the defect this phase exists to
///   fix.
struct SnapshotWriter {
    path: PathBuf,
    next_seq: AtomicU64,
    /// The highest ticket already published, and the write exclusion itself:
    /// held across the write AND the rename, so nothing can be published
    /// between the staleness check and the rename that acts on it.
    published: Mutex<u64>,
}

impl SnapshotWriter {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            // Tickets start at 1 so that 0 can mean "nothing published yet"
            // without the first write comparing equal to it.
            next_seq: AtomicU64::new(1),
            published: Mutex::new(0),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Take a publication ticket. Call this while still holding the lock the
    /// records are read under — a ticket taken afterwards orders WRITERS, which
    /// is not the thing that needs ordering.
    fn ticket(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::SeqCst)
    }

    /// Publish `records` under ticket `seq`, unless something newer is already
    /// on disk.
    async fn publish(&self, seq: u64, records: &[PersistedTask]) -> Result<()> {
        let mut published = self.published.lock().await;
        if *published > seq {
            // Every snapshot is the whole task map, so the newer one on disk
            // already says everything this one would have: writing it is not
            // merely redundant, it is wrong.
            return Ok(());
        }
        write_snapshot(&self.path, &temp_path(&self.path, seq), records).await?;
        *published = seq;
        Ok(())
    }
}

/// The scratch path for one write.
///
/// Unique per write and per process — the ticket separates writers inside this
/// pond, the pid separates two ponds pointed at one data directory. It stays a
/// SIBLING of the published file, because `rename(2)` is only atomic within one
/// filesystem and a scratch file under `/tmp` would not be.
fn temp_path(persist_path: &Path, seq: u64) -> PathBuf {
    let name = persist_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("schedules.json");
    persist_path.with_file_name(format!(".{name}.{}.{seq}.tmp", std::process::id()))
}

/// Write the task list through `tmp`, then publish it with a rename.
async fn write_snapshot(persist_path: &Path, tmp: &Path, records: &[PersistedTask]) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let json = serde_json::to_string_pretty(records)?;
    if let Some(parent) = persist_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent).await?;
    }

    let staged = async {
        let mut file = tokio::fs::File::create(tmp).await?;
        file.write_all(json.as_bytes()).await?;
        // The rename only protects a crash if the bytes reached the device
        // before it. Without this, a power cut on the Jetson's flash can
        // publish an empty or half-written file under the real name, which is
        // the failure write-then-rename is here to prevent.
        file.sync_all().await
    }
    .await;
    if let Err(e) = staged {
        // Nothing was published, so leave nothing behind either.
        let _ = tokio::fs::remove_file(tmp).await;
        return Err(e.into());
    }

    tokio::fs::rename(tmp, persist_path).await?;

    // The rename is a directory change and needs its own flush. It has already
    // taken effect for anything reading the file, so a failure here costs
    // durability across a power cut and nothing else — reporting it as a failed
    // write would have `stamp_fire` warn about a stamp that is on disk.
    if let Some(parent) = persist_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Err(e) = fsync_dir(parent).await {
            tracing::debug!(dir = %parent.display(), error = %e, "could not flush the schedules directory");
        }
    }
    Ok(())
}

/// `fsync` a directory, so a rename inside it survives a power cut.
async fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || std::fs::File::open(dir)?.sync_all())
        .await
        .map_err(std::io::Error::other)?
}

/// Move an unreadable `schedules.json` aside and say where it went.
///
/// Renamed rather than deleted: it is the only copy of the household's
/// automations, and why it stopped parsing is worth being able to look at.
/// `None` means even that failed, and the caller reports it.
async fn quarantine_unreadable(persist_path: &Path) -> Option<PathBuf> {
    let name = persist_path.file_name().and_then(|n| n.to_str())?;
    let kept = persist_path.with_file_name(format!(
        "{name}.unreadable-{}",
        Utc::now().format("%Y%m%dT%H%M%S%.6fZ")
    ));
    match tokio::fs::rename(persist_path, &kept).await {
        Ok(()) => Some(kept),
        Err(e) => {
            tracing::error!(error = %e, "could not move the unreadable schedules file aside");
            None
        }
    }
}

/// Infer `TaskKind` from a legacy `payload` field.
fn migrate_kind_from_payload(record: &PersistedTask) -> TaskKind {
    if let Some(payload) = &record.payload {
        if let Some(url) = payload.get("webhook_url").and_then(|v| v.as_str()) {
            return TaskKind::Webhook {
                webhook_url: url.to_string(),
            };
        }
        if let Some(prompt) = payload.get("prompt").and_then(|v| v.as_str()) {
            return TaskKind::AgentPrompt {
                prompt: prompt.to_string(),
            };
        }
    }
    // Fallback: use the label as a prompt.
    TaskKind::AgentPrompt {
        prompt: record.label.clone(),
    }
}

// ── SchedulerPort impl ────────────────────────────────────────────────────────

#[async_trait]
impl SchedulerPort for CronSchedulerAdapter {
    async fn create_task(&self, req: CreateScheduleRequest) -> Result<Schedule> {
        // A rule that can never fire is refused at the STORE, not at one of the
        // doors. There are four: `POST /rules`, `POST /schedules`, the
        // `create_sensor_rule` MCP tool and the `update` paths — and the MCP
        // tool reaches this port directly without passing through the API at
        // all, so a check that lived only in `routes.rs` would be a check on
        // three doors of four, which is not a check.
        //
        // What that fourth door actually lets through is narrower than the
        // commit adding this said, and the difference is worth writing down:
        // `create_sensor_rule` already rejects a malformed `after`/`before`
        // with the identical `%H:%M` parse, already strips an empty
        // `device_id`/`signal`, and already refuses an empty action list. Of
        // `RuleRejection`'s five classes only two are newly reachable through
        // it — `HalfCondition`, because the tool takes `op` and `value` from
        // independent parameters, and `EmptyAction` for a `Notify` whose title
        // and body are both blank, because those two it passes through
        // unchanged. The seam is right regardless: the tool's own checks are a
        // property of one caller, this is a property of the store.
        validate_kind(&req.kind)?;

        // Reject duplicates
        {
            let guard = self.tasks.lock().await;
            if guard.contains_key(&req.id) {
                bail!("task '{}' already exists", req.id);
            }
        }

        let record = PersistedTask {
            id: req.id.clone(),
            label: req.label.clone(),
            cron: req.cron.clone(),
            timezone: req.timezone.clone(),
            // Anything created through this path carries a zone the caller
            // actually chose, so the legacy backfill must never second-guess
            // it — including a deliberate "UTC".
            tz_migrated: true,
            kind: Some(req.kind.clone()),
            payload: None,
            paused: false,
            created_at: Some(Utc::now()),
            last_run: None,
        };

        // Event-triggered rules (#92) never register a cron job — the rules
        // engine fires them via `run_now` when a matching bus event arrives.
        // The nil job id marks "no cron job", same as the paused state.
        // Arming BEFORE the insert below is deliberate and it is load-bearing
        // in both directions. Arming first means an invalid cron fails the
        // create without leaving a task behind. It also means the first link is
        // armed against a map that does not hold this task yet — and a link
        // that fires and finds no task ends the chain, since that is how delete
        // works. What makes that safe is that `JobScheduler` ticks on a 500 ms
        // sleep, so nothing can fire in the microseconds before the insert.
        // Do not put an await that can block between the two.
        let job_id = if req.kind.is_event_triggered() {
            uuid::Uuid::nil()
        } else {
            self.arm_chain(&req.id, &req.cron, &req.timezone).await?
        };

        let next_run = if req.kind.is_event_triggered() {
            None
        } else {
            compute_next_run(&req.cron, &req.timezone)
        };
        let schedule = Schedule {
            id: req.id.clone(),
            label: req.label,
            cron: req.cron,
            timezone: req.timezone,
            kind: req.kind,
            last_run: None,
            next_run,
            paused: false,
            currently_running: false,
            created_at: record.created_at.unwrap(),
        };

        {
            let mut guard = self.tasks.lock().await;
            guard.insert(
                req.id,
                TaskEntry {
                    persisted: record,
                    job_id,
                    currently_running: false,
                    last_run: None,
                },
            );
        }

        self.save().await?;
        Ok(schedule)
    }

    async fn list_tasks(&self) -> Result<Vec<Schedule>> {
        let guard = self.tasks.lock().await;
        let tasks = guard.values().map(Self::to_schedule).collect();
        Ok(tasks)
    }

    async fn delete_task(&self, id: &str) -> Result<()> {
        let job_id = {
            let mut guard = self.tasks.lock().await;
            let entry = guard
                .remove(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            entry.job_id
        };

        if job_id != uuid::Uuid::nil() {
            self.scheduler.remove(&job_id).await?;
        }

        self.save().await?;
        Ok(())
    }

    async fn pause_task(&self, id: &str) -> Result<()> {
        let job_id = {
            let mut guard = self.tasks.lock().await;
            let entry = guard
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            if entry.persisted.paused {
                return Ok(());
            }
            entry.persisted.paused = true;
            let jid = entry.job_id;
            entry.job_id = uuid::Uuid::nil();
            jid
        };

        if job_id != uuid::Uuid::nil() {
            self.scheduler.remove(&job_id).await?;
        }

        self.save().await?;
        Ok(())
    }

    async fn resume_task(&self, id: &str) -> Result<()> {
        let (cron, timezone, kind) = {
            let guard = self.tasks.lock().await;
            let entry = guard
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            if !entry.persisted.paused {
                return Ok(());
            }
            (
                entry.persisted.cron.clone(),
                entry.persisted.timezone.clone(),
                Self::resolve_kind(entry),
            )
        };

        // Event-triggered rules have no cron job to re-register (#92);
        // clearing `paused` is enough — the rules engine checks the flag.
        let job_id = if kind.is_event_triggered() {
            uuid::Uuid::nil()
        } else {
            self.arm_chain(id, &cron, &timezone).await?
        };

        {
            let mut guard = self.tasks.lock().await;
            if let Some(entry) = guard.get_mut(id) {
                entry.persisted.paused = false;
                entry.job_id = job_id;
            }
        }

        self.save().await?;
        Ok(())
    }

    async fn run_now(&self, id: &str) -> Result<()> {
        use pond_core::user_data::domain::schedule::ScheduleResultEvent;

        let (kind, label) = {
            let guard = self.tasks.lock().await;
            let entry = guard
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            (Self::resolve_kind(entry), entry.persisted.label.clone())
        };

        let executor = self.executor.clone();
        let tasks = self.tasks.clone();
        let run_history = self.run_history.clone();
        let result_tx = self.result_tx.clone();
        let persist = self.persist.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            // Mark running
            {
                let mut guard = tasks.lock().await;
                if let Some(entry) = guard.get_mut(&id) {
                    entry.currently_running = true;
                }
            }
            // Every sensor-rule fire arrives here: the rules engine fires a
            // rule through `run_now`, so this is the stamp its cooldown reads
            // back after a restart.
            Self::stamp_fire(&tasks, &persist, &id).await;

            let run_id = run_history.record_start(&id).await;
            let start = std::time::Instant::now();

            // Broadcast "started" event so clients see progress immediately
            if let Some(tx) = &result_tx {
                let _ = tx.send(ScheduleResultEvent {
                    schedule_id: id.clone(),
                    schedule_label: label.clone(),
                    run_id: run_id.clone(),
                    status: RunStatus::Running,
                    result: None,
                    error: None,
                    duration_ms: None,
                });
            }

            let result = executor.execute(&id, &kind).await;
            let duration_ms = start.elapsed().as_millis() as u64;

            let (status, result_text, error_text) = match &result {
                Ok(text) => {
                    run_history
                        .record_finish(&run_id, RunStatus::Completed, Some(text.clone()), None)
                        .await;
                    (RunStatus::Completed, Some(text.clone()), None)
                }
                Err(e) => {
                    tracing::error!("run_now task {id} failed: {e}");
                    run_history
                        .record_finish(&run_id, RunStatus::Failed, None, Some(e.to_string()))
                        .await;
                    (RunStatus::Failed, None, Some(e.to_string()))
                }
            };

            // Broadcast result event
            if let Some(tx) = &result_tx {
                let _ = tx.send(ScheduleResultEvent {
                    schedule_id: id.clone(),
                    schedule_label: label,
                    run_id: run_id.clone(),
                    status,
                    result: result_text,
                    error: error_text,
                    duration_ms: Some(duration_ms),
                });
            }

            // Mark not-running. `last_run` was stamped at fire time above.
            {
                let mut guard = tasks.lock().await;
                if let Some(entry) = guard.get_mut(&id) {
                    entry.currently_running = false;
                }
            }
        });

        Ok(())
    }

    async fn update_task(&self, id: &str, req: UpdateScheduleRequest) -> Result<Schedule> {
        // Same reasoning as `create_task`: an update is another way to arrive
        // at a stored rule that can never fire.
        if let Some(kind) = &req.kind {
            validate_kind(kind)?;
        }
        let cron_changed = req.cron.is_some();

        // Read current state and apply non-timing changes first.
        let (old_job_id, new_cron, new_timezone, tz_changed, was_paused) = {
            let mut guard = self.tasks.lock().await;
            let entry = guard
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;

            if let Some(label) = &req.label {
                entry.persisted.label = label.clone();
            }
            // Whether the zone actually MOVED, not merely whether one was sent.
            // The UI submits the whole form, so `req.timezone` is `Some` on
            // every edit; re-arming on that would rebuild the chain each time
            // somebody renamed a schedule.
            let tz_changed = req
                .timezone
                .as_ref()
                .is_some_and(|tz| *tz != entry.persisted.timezone);
            if let Some(tz) = &req.timezone {
                entry.persisted.timezone = tz.clone();
            }
            if let Some(kind) = &req.kind {
                entry.persisted.kind = Some(kind.clone());
            }

            let old_job_id = entry.job_id;
            let paused = entry.persisted.paused;
            // Use the NEW cron for scheduling but don't commit it to metadata yet.
            let cron = req.cron.as_ref().unwrap_or(&entry.persisted.cron).clone();
            let timezone = entry.persisted.timezone.clone();
            (old_job_id, cron, timezone, tz_changed, paused)
        };

        // If the TIMING changed and the schedule is active, reschedule the job.
        // The zone belongs in this condition as much as the expression does:
        // "08:00" means a different instant in a different zone, and while the
        // zone was applied to the stored record above it was never applied to
        // the armed job — so moving a schedule from UTC to Africa/Nairobi
        // appeared to work and changed nothing until the next restart.
        //
        // Create the new job FIRST — if it fails (e.g. invalid cron), the old job
        // stays active and the schedule keeps running with the previous cron.
        if (cron_changed || tz_changed) && !was_paused {
            let new_job_id = self.arm_chain(id, &new_cron, &new_timezone).await?;
            // New job created successfully — now safe to remove the old one and commit
            // the cron change to in-memory metadata.
            if old_job_id != uuid::Uuid::nil() {
                let _ = self.scheduler.remove(&old_job_id).await;
            }
            let mut guard = self.tasks.lock().await;
            if let Some(entry) = guard.get_mut(id) {
                entry.persisted.cron = new_cron;
                entry.job_id = new_job_id;
            }
        } else if cron_changed && was_paused {
            // Schedule is paused — just update the stored cron (no active job to replace).
            let mut guard = self.tasks.lock().await;
            if let Some(entry) = guard.get_mut(id) {
                entry.persisted.cron = new_cron;
            }
        }

        self.save().await?;

        let guard = self.tasks.lock().await;
        let entry = guard
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
        Ok(Self::to_schedule(entry))
    }

    async fn get_runs(&self, schedule_id: &str, limit: u32) -> Result<Vec<ScheduleRun>> {
        Ok(self.run_history.get_runs(schedule_id, limit).await)
    }

    async fn list_upcoming(&self, limit: u32) -> Result<Vec<Schedule>> {
        let guard = self.tasks.lock().await;
        let mut schedules: Vec<Schedule> = guard
            .values()
            .filter(|e| !e.persisted.paused)
            .map(Self::to_schedule)
            .collect();
        // Sort by next fire time (soonest first); schedules without next_run sort last.
        schedules.sort_by(|a, b| match (&a.next_run, &b.next_run) {
            (Some(a_next), Some(b_next)) => a_next.cmp(b_next),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.created_at.cmp(&b.created_at),
        });
        schedules.truncate(limit as usize);
        Ok(schedules)
    }

    async fn set_executor(&self, _executor: Arc<dyn ScheduleExecutor>) -> Result<()> {
        // The executor is injected at construction time via `DeferredExecutor`.
        // This method exists on the trait for flexibility but is a no-op here.
        Ok(())
    }
}

// ── Shutdown ──────────────────────────────────────────────────────────────────

impl Drop for CronSchedulerAdapter {
    fn drop(&mut self) {
        let _now = Utc::now(); // suppress unused import warning
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct CountingExecutor(Arc<AtomicU32>);

    #[async_trait]
    impl ScheduleExecutor for CountingExecutor {
        async fn execute(&self, _id: &str, _kind: &TaskKind) -> Result<String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }
    }

    async fn make_scheduler(dir: &std::path::Path) -> CronSchedulerAdapter {
        let counter = Arc::new(AtomicU32::new(0));
        let exec: Arc<dyn ScheduleExecutor> = Arc::new(CountingExecutor(counter));
        CronSchedulerAdapter::new(
            dir.join("schedules.json"),
            dir.join("schedule_runs.json"),
            exec,
        )
        .await
        .expect("scheduler init failed")
    }

    /// A scheduler whose household zone is `tz`, for the backfill tests.
    async fn make_scheduler_in(dir: &std::path::Path, tz: &str) -> CronSchedulerAdapter {
        let counter = Arc::new(AtomicU32::new(0));
        let exec: Arc<dyn ScheduleExecutor> = Arc::new(CountingExecutor(counter));
        CronSchedulerAdapter::with_options(
            dir.join("schedules.json"),
            dir.join("schedule_runs.json"),
            exec,
            None,
            50,
            tz,
        )
        .await
        .expect("scheduler init failed")
    }

    /// Write a snapshot as a pond running the pre-fix code would have: a zone
    /// field present, and no `tz_migrated` key at all.
    async fn write_legacy_snapshot(dir: &std::path::Path, id: &str, timezone: &str) {
        let record = PersistedTask {
            id: id.to_string(),
            label: "Morning briefing".into(),
            cron: "0 0 8 * * *".into(),
            timezone: timezone.to_string(),
            tz_migrated: false,
            kind: Some(TaskKind::AgentPrompt {
                prompt: "brief me".into(),
            }),
            payload: None,
            paused: false,
            created_at: Some(Utc::now()),
            last_run: None,
        };

        // Serialize a real record, then delete the key outright — a pre-fix
        // pond wrote a file with no `tz_migrated` at all, and `false` and
        // absent must be indistinguishable here or the test is checking the
        // wrong thing.
        let mut value = serde_json::to_value([record]).unwrap();
        value[0]
            .as_object_mut()
            .unwrap()
            .remove("tz_migrated")
            .expect("the marker should have been serialized before removal");

        tokio::fs::write(
            dir.join("schedules.json"),
            serde_json::to_string(&value).unwrap(),
        )
        .await
        .unwrap();
    }

    async fn stored_timezone(dir: &std::path::Path, id: &str) -> String {
        let raw = tokio::fs::read_to_string(dir.join("schedules.json"))
            .await
            .unwrap();
        let records: Vec<PersistedTask> = serde_json::from_str(&raw).unwrap();
        records
            .into_iter()
            .find(|r| r.id == id)
            .expect("task missing from snapshot")
            .timezone
    }

    fn create_req(id: &str, cron: &str) -> CreateScheduleRequest {
        CreateScheduleRequest {
            id: id.to_string(),
            label: format!("Test task {id}"),
            cron: cron.to_string(),
            timezone: "UTC".to_string(),
            kind: TaskKind::AgentPrompt {
                prompt: "Hello".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn create_and_list() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("t1", "0 0 0 * * *"))
            .await
            .unwrap();
        sched
            .create_task(create_req("t2", "0 0 1 * * *"))
            .await
            .unwrap();

        let tasks = sched.list_tasks().await.unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().any(|t| t.id == "t1"));
        assert!(tasks.iter().any(|t| t.id == "t2"));
    }

    #[tokio::test]
    async fn delete_task() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("del", "0 0 2 * * *"))
            .await
            .unwrap();
        sched.delete_task("del").await.unwrap();

        let tasks = sched.list_tasks().await.unwrap();
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn delete_nonexistent_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        assert!(sched.delete_task("ghost").await.is_err());
    }

    #[tokio::test]
    async fn pause_and_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("p1", "0 0 3 * * *"))
            .await
            .unwrap();
        sched.pause_task("p1").await.unwrap();

        let tasks = sched.list_tasks().await.unwrap();
        assert!(tasks.iter().find(|t| t.id == "p1").unwrap().paused);

        sched.resume_task("p1").await.unwrap();
        let tasks = sched.list_tasks().await.unwrap();
        assert!(!tasks.iter().find(|t| t.id == "p1").unwrap().paused);
    }

    #[tokio::test]
    async fn invalid_cron_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        let err = sched.create_task(create_req("bad", "not a cron")).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn persistence_round_trip() {
        let tmp = tempfile::tempdir().unwrap();

        {
            let sched = make_scheduler(tmp.path()).await;
            sched
                .create_task(create_req("persist1", "0 0 4 * * *"))
                .await
                .unwrap();
        }

        // Rehydrate from disk
        let sched2 = make_scheduler(tmp.path()).await;
        let tasks = sched2.list_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "persist1");
        assert_eq!(tasks[0].timezone, "UTC");
    }

    #[tokio::test]
    async fn duplicate_id_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        sched
            .create_task(create_req("dup", "0 0 5 * * *"))
            .await
            .unwrap();
        assert!(sched
            .create_task(create_req("dup", "0 0 6 * * *"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn next_run_is_populated() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        let schedule = sched
            .create_task(create_req("nr1", "0 0 12 * * *"))
            .await
            .unwrap();

        // next_run should be populated for an active schedule.
        assert!(
            schedule.next_run.is_some(),
            "next_run should be computed for an active schedule"
        );

        // The next run should be in the future.
        let next = schedule.next_run.unwrap();
        assert!(
            next > chrono::Utc::now(),
            "next_run should be in the future, got {next}"
        );
    }

    #[tokio::test]
    async fn paused_schedule_has_no_next_run() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("pnr", "0 0 12 * * *"))
            .await
            .unwrap();
        sched.pause_task("pnr").await.unwrap();

        let tasks = sched.list_tasks().await.unwrap();
        let task = tasks.iter().find(|t| t.id == "pnr").unwrap();
        assert!(
            task.next_run.is_none(),
            "paused schedule should have no next_run"
        );
    }

    #[tokio::test]
    async fn list_upcoming_sorted_by_next_run() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        // Create two tasks: one fires at noon, the other at 6am.
        // The 6am task should sort before the noon task.
        sched
            .create_task(create_req("noon", "0 0 12 * * *"))
            .await
            .unwrap();
        sched
            .create_task(create_req("morning", "0 0 6 * * *"))
            .await
            .unwrap();

        let upcoming = sched.list_upcoming(10).await.unwrap();
        assert_eq!(upcoming.len(), 2);

        // Both should have next_run populated.
        assert!(upcoming[0].next_run.is_some());
        assert!(upcoming[1].next_run.is_some());

        // First should fire before or equal to the second.
        assert!(
            upcoming[0].next_run.unwrap() <= upcoming[1].next_run.unwrap(),
            "upcoming schedules should be sorted by next_run"
        );
    }

    #[test]
    fn compute_next_run_valid_cron() {
        let next = super::compute_next_run("0 0 12 * * *", "UTC");
        assert!(next.is_some(), "valid cron should produce a next_run");
        assert!(next.unwrap() > chrono::Utc::now());
    }

    #[test]
    fn compute_next_run_invalid_cron() {
        let next = super::compute_next_run("not a cron", "UTC");
        assert!(next.is_none(), "invalid cron should return None");
    }

    // ── Timezone: the schedules-fire-in-UTC defect ───────────────────────

    /// The regression test for the reported bug: 8:00 AM in Nairobi is 05:00
    /// UTC, and the old code armed 08:00 UTC — which arrives at 11:00 local,
    /// exactly the three hours late that was reported.
    ///
    /// It asserts the UTC INSTANT deliberately. Asserting the local hour would
    /// pass against the broken code too, because the broken code also produced
    /// a time whose local hour was 8 — in the wrong zone.
    #[test]
    fn eight_am_in_nairobi_is_five_am_utc() {
        let from = "2026-08-13T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let next =
            super::next_occurrence_after("0 0 8 * * *", chrono_tz::Tz::Africa__Nairobi, from)
                .expect("a daily 08:00 rule has an occurrence");

        assert_eq!(next.to_rfc3339(), "2026-08-13T05:00:00+00:00");
    }

    /// The same expression in UTC still means 08:00 UTC — the fix must not
    /// shift schedules that really were UTC.
    #[test]
    fn eight_am_in_utc_is_unchanged() {
        let from = "2026-08-13T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let next = super::next_occurrence_after("0 0 8 * * *", chrono_tz::Tz::UTC, from)
            .expect("a daily 08:00 rule has an occurrence");

        assert_eq!(next.to_rfc3339(), "2026-08-13T08:00:00+00:00");
    }

    /// The reason this is a one-shot chain rather than `Job::new_async_tz`.
    ///
    /// New York is UTC-5 in winter and UTC-4 in summer. A fixed offset captured
    /// at registration — which is what `new_async_tz` stores — is necessarily
    /// wrong on one side of a transition. Recomputing per occurrence keeps the
    /// LOCAL hour fixed at 08:00, which is what the household asked for.
    #[test]
    fn a_daily_local_hour_survives_a_dst_transition() {
        let tz = chrono_tz::Tz::America__New_York;
        let winter = "2026-01-15T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let summer = "2026-07-15T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let in_winter = super::next_occurrence_after("0 0 8 * * *", tz, winter).unwrap();
        let in_summer = super::next_occurrence_after("0 0 8 * * *", tz, summer).unwrap();

        // Same local hour on both sides …
        assert_eq!(
            in_winter.with_timezone(&tz).format("%H:%M").to_string(),
            "08:00"
        );
        assert_eq!(
            in_summer.with_timezone(&tz).format("%H:%M").to_string(),
            "08:00"
        );

        // … which means DIFFERENT UTC instants. A fixed offset cannot do both,
        // and this inequality is the whole argument for the chain.
        assert_eq!(in_winter.format("%H:%M").to_string(), "13:00");
        assert_eq!(in_summer.format("%H:%M").to_string(), "12:00");
    }

    /// An unresolvable zone must not take the schedule down with it.
    #[test]
    fn an_unknown_timezone_falls_back_to_utc() {
        assert_eq!(super::parse_tz("Mars/Olympus_Mons"), chrono_tz::Tz::UTC);
        assert_eq!(super::parse_tz(""), chrono_tz::Tz::UTC);
        assert_eq!(
            super::parse_tz("Africa/Nairobi"),
            chrono_tz::Tz::Africa__Nairobi
        );
    }

    /// `next_run` must be computed in the schedule's zone too. It was not, and
    /// because it agreed with the equally-wrong fire time, the UI looked
    /// consistent while the schedule ran three hours late.
    #[test]
    fn next_run_is_computed_in_the_schedules_zone() {
        let utc = super::compute_next_run("0 0 8 * * *", "UTC").unwrap();
        let nairobi = super::compute_next_run("0 0 8 * * *", "Africa/Nairobi").unwrap();

        assert_ne!(
            utc, nairobi,
            "08:00 UTC and 08:00 in Nairobi are three hours apart; next_run must reflect that"
        );
    }

    // ── The one-time legacy backfill ─────────────────────────────────────

    #[tokio::test]
    async fn a_legacy_schedule_is_migrated_to_the_household_zone() {
        let tmp = tempfile::tempdir().unwrap();
        write_legacy_snapshot(tmp.path(), "sched-morning", "UTC").await;

        let sched = make_scheduler_in(tmp.path(), "Africa/Nairobi").await;
        let tasks = sched.list_tasks().await.unwrap();

        assert_eq!(tasks[0].timezone, "Africa/Nairobi");
        assert_eq!(
            stored_timezone(tmp.path(), "sched-morning").await,
            "Africa/Nairobi",
            "the migration must reach disk, or it runs again every boot"
        );
    }

    /// The marker is what makes this a migration and not a standing override.
    #[tokio::test]
    async fn a_deliberate_utc_survives_a_second_start() {
        let tmp = tempfile::tempdir().unwrap();
        write_legacy_snapshot(tmp.path(), "sched-morning", "UTC").await;

        // First start migrates it and marks it.
        drop(make_scheduler_in(tmp.path(), "Africa/Nairobi").await);

        // The household then deliberately puts this one back to UTC.
        let raw = tokio::fs::read_to_string(tmp.path().join("schedules.json"))
            .await
            .unwrap();
        let mut records: Vec<PersistedTask> = serde_json::from_str(&raw).unwrap();
        records[0].timezone = "UTC".into();
        tokio::fs::write(
            tmp.path().join("schedules.json"),
            serde_json::to_string(&records).unwrap(),
        )
        .await
        .unwrap();

        // A later start must leave that choice alone.
        let sched = make_scheduler_in(tmp.path(), "Africa/Nairobi").await;
        let tasks = sched.list_tasks().await.unwrap();
        assert_eq!(
            tasks[0].timezone, "UTC",
            "a zone chosen after the migration must not be overwritten"
        );
    }

    /// A schedule that already names a real zone is not a legacy row.
    #[tokio::test]
    async fn a_schedule_with_a_real_zone_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        write_legacy_snapshot(tmp.path(), "sched-morning", "Europe/Berlin").await;

        let sched = make_scheduler_in(tmp.path(), "Africa/Nairobi").await;
        let tasks = sched.list_tasks().await.unwrap();

        assert_eq!(tasks[0].timezone, "Europe/Berlin");
    }

    /// Before onboarding sets a zone there is nothing to migrate TO, and
    /// burning the marker there would mean the backfill never runs later.
    #[tokio::test]
    async fn a_utc_household_defers_the_migration() {
        let tmp = tempfile::tempdir().unwrap();
        write_legacy_snapshot(tmp.path(), "sched-morning", "UTC").await;

        // Boot while the household zone is still the default.
        drop(make_scheduler_in(tmp.path(), "UTC").await);

        // Onboarding lands, and the NEXT boot migrates.
        let sched = make_scheduler_in(tmp.path(), "Africa/Nairobi").await;
        let tasks = sched.list_tasks().await.unwrap();
        assert_eq!(
            tasks[0].timezone, "Africa/Nairobi",
            "the migration must still be pending after a UTC-household boot"
        );
    }

    /// A schedule created through the API carries a chosen zone, so the legacy
    /// backfill must never touch it — even when that choice is UTC.
    #[tokio::test]
    async fn a_newly_created_schedule_is_not_a_migration_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let sched = make_scheduler_in(tmp.path(), "Africa/Nairobi").await;
            sched
                .create_task(create_req("sched-new", "0 0 8 * * *"))
                .await
                .unwrap();
        }

        let sched = make_scheduler_in(tmp.path(), "Africa/Nairobi").await;
        let tasks = sched.list_tasks().await.unwrap();
        assert_eq!(
            tasks[0].timezone, "UTC",
            "create_req asks for UTC explicitly; the backfill must respect it"
        );
    }

    // ── The one-shot chain actually fires, and actually stops ────────────

    /// A scheduler plus the fire counter its executor increments.
    async fn make_counting_scheduler(
        dir: &std::path::Path,
    ) -> (CronSchedulerAdapter, Arc<AtomicU32>) {
        let counter = Arc::new(AtomicU32::new(0));
        let exec: Arc<dyn ScheduleExecutor> = Arc::new(CountingExecutor(counter.clone()));
        let sched = CronSchedulerAdapter::with_options(
            dir.join("schedules.json"),
            dir.join("schedule_runs.json"),
            exec,
            None,
            50,
            "UTC",
        )
        .await
        .expect("scheduler init failed");
        (sched, counter)
    }

    /// A recurring cron job repeats on its own; a one-shot does not. The whole
    /// design rests on each link arming the next, so "it fired MORE THAN ONCE"
    /// is the property under test — a single fire would mean the chain died
    /// after its first link and every schedule silently became one-shot.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_chain_re_arms_itself_after_each_fire() {
        let tmp = tempfile::tempdir().unwrap();
        let (sched, fires) = make_counting_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("sched-tick", "* * * * * *"))
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(3_500)).await;

        assert!(
            fires.load(Ordering::SeqCst) >= 2,
            "a chained schedule must fire repeatedly; saw {} fire(s)",
            fires.load(Ordering::SeqCst)
        );
    }

    /// Deleting a schedule has to break the chain. Because re-arming now
    /// happens INSIDE the fire path, a link that does not check whether its
    /// task still exists would keep arming successors forever — a deleted
    /// schedule that goes on running with no way to stop it short of a restart.
    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_schedule_breaks_the_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let (sched, fires) = make_counting_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("sched-tick", "* * * * * *"))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
        assert!(
            fires.load(Ordering::SeqCst) >= 1,
            "should have fired at least once"
        );

        sched.delete_task("sched-tick").await.unwrap();
        // Let any in-flight link finish before taking the reading.
        tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
        let after_delete = fires.load(Ordering::SeqCst);

        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
        assert_eq!(
            fires.load(Ordering::SeqCst),
            after_delete,
            "a deleted schedule must not fire again"
        );
    }

    /// Same property for pause, which leaves the task in the map rather than
    /// removing it — so it exercises the paused branch of the terminator.
    #[tokio::test(flavor = "multi_thread")]
    async fn pausing_a_schedule_breaks_the_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let (sched, fires) = make_counting_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("sched-tick", "* * * * * *"))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;

        sched.pause_task("sched-tick").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
        let after_pause = fires.load(Ordering::SeqCst);

        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
        assert_eq!(
            fires.load(Ordering::SeqCst),
            after_pause,
            "a paused schedule must not fire again"
        );
    }

    /// Changing only the zone is a timing change. It updated the stored record
    /// but never re-registered the job, so the schedule went on firing at the
    /// old zone's instant until something else restarted the pond.
    #[tokio::test]
    async fn changing_only_the_timezone_reschedules() {
        use pond_core::user_data::ports::scheduler::UpdateScheduleRequest;

        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        sched
            .create_task(create_req("sched-1", "0 0 8 * * *"))
            .await
            .unwrap();

        let before = sched.list_tasks().await.unwrap()[0].next_run.unwrap();

        let updated = sched
            .update_task(
                "sched-1",
                UpdateScheduleRequest {
                    timezone: Some("Africa/Nairobi".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.timezone, "Africa/Nairobi");
        assert_ne!(
            updated.next_run.unwrap(),
            before,
            "moving the zone must move the next fire time"
        );
    }

    #[tokio::test]
    async fn update_task_changes_fields() {
        use pond_core::user_data::ports::scheduler::UpdateScheduleRequest;

        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("upd1", "0 0 8 * * *"))
            .await
            .unwrap();

        let updated = sched
            .update_task(
                "upd1",
                UpdateScheduleRequest {
                    label: Some("Updated label".to_string()),
                    cron: Some("0 30 9 * * *".to_string()),
                    timezone: Some("Africa/Nairobi".to_string()),
                    kind: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.label, "Updated label");
        assert_eq!(updated.cron, "0 30 9 * * *");
        assert_eq!(updated.timezone, "Africa/Nairobi");
        // Kind unchanged
        match &updated.kind {
            TaskKind::AgentPrompt { prompt } => assert_eq!(prompt, "Hello"),
            other => panic!("expected AgentPrompt, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_with_invalid_cron_preserves_old_schedule() {
        use pond_core::user_data::ports::scheduler::UpdateScheduleRequest;

        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched
            .create_task(create_req("atomic1", "0 0 8 * * *"))
            .await
            .unwrap();

        // Attempt to update with an invalid cron — should fail without breaking the schedule.
        let result = sched
            .update_task(
                "atomic1",
                UpdateScheduleRequest {
                    label: None,
                    cron: Some("every morning at 9".to_string()),
                    timezone: None,
                    kind: None,
                },
            )
            .await;

        assert!(result.is_err(), "invalid cron should produce an error");

        // The schedule should still exist with the ORIGINAL cron.
        let tasks = sched.list_tasks().await.unwrap();
        let task = tasks.iter().find(|t| t.id == "atomic1").unwrap();
        assert_eq!(
            task.cron, "0 0 8 * * *",
            "original cron should be preserved"
        );
        assert!(!task.paused, "schedule should still be active");
    }

    #[tokio::test]
    async fn update_nonexistent_errors() {
        use pond_core::user_data::ports::scheduler::UpdateScheduleRequest;

        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        assert!(sched
            .update_task("ghost", UpdateScheduleRequest::default())
            .await
            .is_err());
    }

    // ── Rules that can never fire are refused at the STORE (PAI-7 P8) ────

    fn rule_req(
        id: &str,
        actions: Vec<pond_core::user_data::domain::schedule::TriggerAction>,
    ) -> CreateScheduleRequest {
        use pond_core::user_data::domain::schedule::{
            SensorTriggerSpec, TriggerCondition, TriggerSource, TriggerSourceKind,
        };
        CreateScheduleRequest {
            id: id.to_string(),
            label: format!("rule {id}"),
            cron: "@event".to_string(),
            timezone: "UTC".to_string(),
            kind: TaskKind::SensorTrigger(SensorTriggerSpec {
                source: TriggerSource {
                    kind: TriggerSourceKind::Sensor,
                    device_id: Some("backyard-pir".into()),
                    signal: Some("motion".into()),
                },
                condition: TriggerCondition::default(),
                actions,
                cooldown_secs: 60,
            }),
        }
    }

    fn notify() -> Vec<pond_core::user_data::domain::schedule::TriggerAction> {
        vec![
            pond_core::user_data::domain::schedule::TriggerAction::Notify {
                title: "Motion".into(),
                body: "Backyard".into(),
            },
        ]
    }

    #[tokio::test]
    async fn a_rule_that_can_never_fire_is_refused_at_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        // The control first, so "refused" below is about the SPEC and not about
        // the fixture or the adapter.
        sched.create_task(rule_req("good", notify())).await.unwrap();

        let err = sched
            .create_task(rule_req("actionless", vec![]))
            .await
            .expect_err(
                "a rule with no actions was stored: the MCP tool writes through this port                  without passing the API, so a check that lives only in routes.rs lets the                  model create the rules a person is refused",
            );
        assert!(err.to_string().contains("action"), "{err}");
        assert_eq!(sched.list_tasks().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_update_cannot_smuggle_in_a_rule_that_can_never_fire() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        sched.create_task(rule_req("r", notify())).await.unwrap();

        let bad = rule_req("r", vec![]).kind;
        let err = sched
            .update_task(
                "r",
                UpdateScheduleRequest {
                    kind: Some(bad),
                    ..Default::default()
                },
            )
            .await
            .expect_err("update is another door onto a stored rule");
        assert!(err.to_string().contains("action"), "{err}");

        // And the good rule is still the one stored.
        let tasks = sched.list_tasks().await.unwrap();
        match &tasks[0].kind {
            TaskKind::SensorTrigger(spec) => assert_eq!(spec.actions.len(), 1),
            other => panic!("expected the rule, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_rule_stored_before_the_check_existed_still_loads() {
        // Rehydration deliberately does NOT validate. Refusing to load would
        // delete somebody's automation on upgrade, which is a worse answer than
        // keeping a rule that does nothing — and they can now see why it does
        // nothing, because an edit to it is refused with the reason.
        let tmp = tempfile::tempdir().unwrap();
        let stored = serde_json::json!([{
            "id": "legacy-rule",
            "label": "Actionless",
            "cron": "@event",
            "timezone": "UTC",
            "kind": {"type": "sensor_trigger", "source": {"kind": "sensor"}, "actions": []},
            "paused": false
        }]);
        tokio::fs::write(
            tmp.path().join("schedules.json"),
            serde_json::to_string_pretty(&stored).unwrap(),
        )
        .await
        .unwrap();

        let sched = make_scheduler(tmp.path()).await;
        let tasks = sched.list_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1, "an existing rule must survive the upgrade");
    }

    // ── The file every schedule lives in (PAI-7 P8 repair) ───────────────

    fn snapshot_of(ids: &[&str], pad: usize) -> Vec<PersistedTask> {
        ids.iter()
            .map(|id| PersistedTask {
                id: (*id).to_string(),
                label: format!("label of {id} {}", "x".repeat(pad)),
                cron: "0 0 4 * * *".into(),
                timezone: "UTC".into(),
                tz_migrated: true,
                kind: Some(TaskKind::AgentPrompt {
                    prompt: "back up".into(),
                }),
                payload: None,
                paused: false,
                created_at: Some(Utc::now()),
                last_run: None,
            })
            .collect()
    }

    #[test]
    fn no_two_writes_share_a_scratch_file() {
        // The corruption this replaces: one fixed `schedules.json.tmp` for
        // every writer, so two of them truncated and wrote the same inode and
        // the rename published the mixture. The rename was always atomic; the
        // write into the file it renames was the unguarded step.
        let target = std::path::Path::new("/var/pond/schedules.json");
        let a = temp_path(target, 1);
        let b = temp_path(target, 2);
        assert_ne!(
            a, b,
            "two writes share a scratch file: whichever renames first \
             publishes a mixture of both, and the other goes on writing into \
             the live schedules.json afterwards"
        );

        for p in [&a, &b] {
            // `rename(2)` is only atomic within one filesystem, so the scratch
            // file has to be a sibling of the file it becomes.
            assert_eq!(
                p.parent(),
                target.parent(),
                "the scratch file left the published file's directory: the \
                 rename is no longer guaranteed to be atomic"
            );
            assert_ne!(p, target, "the scratch file IS the published file");
        }
    }

    #[tokio::test]
    async fn a_stale_snapshot_does_not_overwrite_a_newer_one() {
        // Two writers publishing in the reverse of the order their contents
        // were read — a fire stamp racing an API edit is exactly this. Without
        // the ordering the older snapshot lands last and the newest fire stamp
        // is gone, which is the defect the phase exists to fix.
        let tmp = tempfile::tempdir().unwrap();
        let writer = SnapshotWriter::new(tmp.path().join("schedules.json"));

        let old_seq = writer.ticket();
        let new_seq = writer.ticket();
        let newer = snapshot_of(&["a", "b"], 4);
        let older = snapshot_of(&["a"], 4);

        writer.publish(new_seq, &newer).await.unwrap();
        writer.publish(old_seq, &older).await.unwrap();

        let on_disk = tokio::fs::read_to_string(writer.path()).await.unwrap();
        let records: Vec<PersistedTask> = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(
            records.len(),
            2,
            "the older snapshot was published over the newer one: {on_disk}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_writers_leave_one_whole_snapshot() {
        // The production shape: `rules_engine` fires EVERY rule matching one
        // event through its own `run_now` task, and any of those can land on an
        // API create or pause. The two payloads differ in size so a torn file
        // cannot pass by being the right length.
        //
        // This one is a smoke test and says so: it can only catch a tear it
        // happens to hit. What carries the property is
        // `no_two_writes_share_a_scratch_file` above — with a scratch file per
        // write there is no shared inode left to tear.
        let tmp = tempfile::tempdir().unwrap();
        let writer = Arc::new(SnapshotWriter::new(tmp.path().join("schedules.json")));

        let mut handles = Vec::new();
        let mut expected = 0usize;
        for i in 0..32u64 {
            // The LAST ticket is the big snapshot, so "which one won" is a
            // question with a visible answer.
            let records = if i % 2 == 1 {
                snapshot_of(&["a", "b", "c", "d", "e", "f", "g", "h"], 512)
            } else {
                snapshot_of(&["a"], 1)
            };
            let seq = writer.ticket();
            expected = records.len();
            let w = writer.clone();
            handles.push(tokio::spawn(async move {
                w.publish(seq, &records).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let on_disk = tokio::fs::read_to_string(writer.path()).await.unwrap();
        let records: Vec<PersistedTask> = serde_json::from_str(&on_disk).unwrap_or_else(|e| {
            panic!(
                "schedules.json does not parse after concurrent writes ({e}): that is \
                 every schedule and every rule in the pond, not one cooldown. The file \
                 was {} bytes",
                on_disk.len()
            )
        });
        assert_eq!(
            records.len(),
            expected,
            "the last-read snapshot is not the one on disk"
        );
        // Vacuity control: the two payloads really do differ in length, so the
        // assertion above is about WHICH snapshot won and not about a shape
        // both of them share.
        assert_ne!(snapshot_of(&["a"], 1).len(), expected);

        // And nothing was left behind for the next writer to find.
        let mut leftovers = tokio::fs::read_dir(tmp.path()).await.unwrap();
        while let Some(e) = leftovers.next_entry().await.unwrap() {
            let name = e.file_name().to_string_lossy().to_string();
            assert!(!name.ends_with(".tmp"), "scratch file left behind: {name}");
        }
    }

    #[tokio::test]
    async fn an_unreadable_schedules_file_is_kept_and_the_pond_still_starts() {
        // `pond-server` turns a constructor Err into `scheduler = None`, so
        // returning one here means 503 from every schedule endpoint for the
        // life of the install and the same again on the next boot. Start empty
        // instead — but never by deleting the only copy of the household's
        // automations.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("schedules.json");
        let corrupt = "[{\"id\":\"nightly-backup\",\"label\":\"Backup\"}, {\"id\":";
        tokio::fs::write(&path, corrupt).await.unwrap();

        let counter = Arc::new(AtomicU32::new(0));
        let exec: Arc<dyn ScheduleExecutor> = Arc::new(CountingExecutor(counter));
        let sched = CronSchedulerAdapter::new(path.clone(), tmp.path().join("runs.json"), exec)
            .await
            .expect(
                "an unreadable schedules.json failed the whole scheduler: pond-server \
                 turns that into `scheduler = None`, so every schedule endpoint answers \
                 503 for the life of the install and the next boot does it again",
            );
        assert!(sched.list_tasks().await.unwrap().is_empty());

        let mut kept = None;
        let mut entries = tokio::fs::read_dir(tmp.path()).await.unwrap();
        while let Some(e) = entries.next_entry().await.unwrap() {
            if e.file_name()
                .to_string_lossy()
                .contains("schedules.json.unreadable-")
            {
                kept = Some(e.path());
            }
        }
        let kept = kept.expect("the unreadable file must be kept, not deleted");
        assert_eq!(
            tokio::fs::read_to_string(&kept).await.unwrap(),
            corrupt,
            "the quarantined copy is not the file that failed to parse"
        );
    }

    #[tokio::test]
    async fn a_paused_rules_fire_stamp_survives_a_restart() {
        // The fixture is what `pause_task` writes after a fire: paused, with
        // the stamp of the last one. Rehydration used to carry that stamp on
        // two separate branches and only the active branch had a test, so the
        // paused copy could be dropped with the suite green — and then a pause,
        // a deploy and a resume put an hour-long cooldown back to "never
        // fired", which is PAI-7 P8's whole defect on the pause path.
        let tmp = tempfile::tempdir().unwrap();
        let fired_at = Utc::now() - chrono::Duration::seconds(45);
        let stored = serde_json::json!([{
            "id": "paused-rule",
            "label": "Backyard motion",
            "cron": "@event",
            "timezone": "UTC",
            "kind": {
                "type": "sensor_trigger",
                "source": {"kind": "sensor", "device_id": "backyard-pir", "signal": "motion"},
                "actions": [{"type": "notify", "title": "Motion", "body": "Backyard"}],
                "cooldown_secs": 3600
            },
            "paused": true,
            "last_run": fired_at,
        }]);
        tokio::fs::write(
            tmp.path().join("schedules.json"),
            serde_json::to_string_pretty(&stored).unwrap(),
        )
        .await
        .unwrap();

        let sched = make_scheduler(tmp.path()).await;
        let tasks = sched.list_tasks().await.unwrap();
        let rule = tasks
            .iter()
            .find(|t| t.id == "paused-rule")
            .expect("the paused rule survives the restart");
        assert!(
            rule.paused,
            "the fixture has to BE paused or it is not testing the paused path"
        );
        assert_eq!(
            rule.last_run,
            Some(fired_at),
            "rehydration dropped a PAUSED rule's fire stamp: resume it and the \
             hour-long cooldown reads as never-fired, so the next matching \
             event fires it"
        );

        // And a resume keeps it — `resume_task` rewrites the file.
        sched.resume_task("paused-rule").await.unwrap();
        let after = sched.list_tasks().await.unwrap();
        let rule = after.iter().find(|t| t.id == "paused-rule").unwrap();
        assert!(!rule.paused);
        assert_eq!(rule.last_run, Some(fired_at), "resume dropped the stamp");
    }

    #[tokio::test]
    async fn backward_compat_legacy_payload() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a legacy schedules.json without kind/timezone fields.
        let legacy = serde_json::json!([{
            "id": "legacy1",
            "label": "Old webhook task",
            "cron": "0 0 8 * * *",
            "payload": {"webhook_url": "https://example.com/hook"},
            "paused": false
        }]);
        tokio::fs::write(
            tmp.path().join("schedules.json"),
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .await
        .unwrap();

        let sched = make_scheduler(tmp.path()).await;
        let tasks = sched.list_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "legacy1");
        assert_eq!(tasks[0].timezone, "UTC"); // default
        match &tasks[0].kind {
            TaskKind::Webhook { webhook_url } => {
                assert_eq!(webhook_url, "https://example.com/hook");
            }
            other => panic!("expected Webhook kind, got {other:?}"),
        }
    }
}
