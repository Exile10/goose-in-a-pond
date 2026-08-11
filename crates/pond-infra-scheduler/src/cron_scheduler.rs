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
use chrono::Utc;
use pond_core::user_data::domain::schedule::{RunStatus, Schedule, ScheduleRun, TaskKind};
use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
use pond_core::user_data::ports::scheduler::{
    CreateScheduleRequest, SchedulerPort, UpdateScheduleRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
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
    /// When this task last FIRED (not when it finished). Written for the kinds
    /// whose debounce reads it back — see [`durable_fire_stamp`] — so a sensor
    /// rule's cooldown survives a restart. `None` on a file written before
    /// PAI-7 P8, which reads as "never fired": the first event after an upgrade
    /// fires once, and the stamp exists from then on.
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
    persist_path: PathBuf,
    executor: Arc<dyn ScheduleExecutor>,
    run_history: Arc<JsonRunHistory>,
    /// Optional broadcast sender for schedule result events (SSE delivery).
    result_tx: Option<
        tokio::sync::broadcast::Sender<pond_core::user_data::domain::schedule::ScheduleResultEvent>,
    >,
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
        Self::with_options(persist_path, runs_path, executor, None, 50).await
    }

    /// Create with all options.
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
    ) -> Result<Self> {
        let scheduler = JobScheduler::new().await?;
        let tasks: Arc<Mutex<HashMap<String, TaskEntry>>> = Arc::new(Mutex::new(HashMap::new()));
        let run_history = Arc::new(JsonRunHistory::new(runs_path, max_runs_per_task).await?);

        let adapter = Self {
            scheduler,
            tasks,
            persist_path,
            executor,
            run_history,
            result_tx,
        };

        // Rehydrate persisted tasks
        adapter.rehydrate().await?;

        adapter.scheduler.start().await?;

        Ok(adapter)
    }

    // ── Persistence helpers ───────────────────────────────────────────────────

    async fn save(&self) -> Result<()> {
        let guard = self.tasks.lock().await;
        let records: Vec<PersistedTask> = guard.values().map(|e| e.persisted.clone()).collect();
        drop(guard);

        write_snapshot(&self.persist_path, &records).await
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
        persist_path: &std::path::Path,
        id: &str,
    ) {
        let now = Utc::now();
        let records: Vec<PersistedTask> = {
            let mut guard = tasks.lock().await;
            let Some(entry) = guard.get_mut(id) else {
                return;
            };
            entry.last_run = Some(now);
            entry.persisted.last_run = Some(now);
            if !durable_fire_stamp(&Self::resolve_kind(entry)) {
                return;
            }
            guard.values().map(|e| e.persisted.clone()).collect()
        };

        if let Err(e) = write_snapshot(persist_path, &records).await {
            // A lost stamp re-fires the rule after the next restart, which is
            // the defect this exists to fix — so it is a warning, not a debug.
            tracing::warn!(task = %id, error = %e, "could not persist fire stamp");
        }
    }

    async fn rehydrate(&self) -> Result<()> {
        if !self.persist_path.exists() {
            return Ok(());
        }

        let json = tokio::fs::read_to_string(&self.persist_path).await?;
        let records: Vec<PersistedTask> = serde_json::from_str(&json)?;

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

            if record.paused {
                let mut guard = self.tasks.lock().await;
                let last_run = record.last_run;
                guard.insert(
                    record.id.clone(),
                    TaskEntry {
                        persisted: record,
                        job_id: uuid::Uuid::nil(),
                        currently_running: false,
                        last_run,
                    },
                );
                continue;
            }

            let kind = record.kind.clone().unwrap();
            // Event-triggered rules (#92) are loaded but never cron-registered.
            let job_id = if kind.is_event_triggered() {
                uuid::Uuid::nil()
            } else {
                self.add_job_to_scheduler(&record.id, &record.cron, kind)
                    .await?
            };

            let mut guard = self.tasks.lock().await;
            // The persisted fire stamp is the whole point of PAI-7 P8's first
            // repair: dropping it here would put the cooldown back in memory.
            let last_run = record.last_run;
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

    async fn add_job_to_scheduler(
        &self,
        task_id: &str,
        cron: &str,
        kind: TaskKind,
    ) -> Result<uuid::Uuid> {
        use pond_core::user_data::domain::schedule::ScheduleResultEvent;

        let executor = self.executor.clone();
        let tasks = self.tasks.clone();
        let run_history = self.run_history.clone();
        let result_tx = self.result_tx.clone();
        let persist_path = self.persist_path.clone();
        let id = task_id.to_string();

        let job = Job::new_async(cron, move |_uuid, _lock| {
            let executor = executor.clone();
            let tasks = tasks.clone();
            let run_history = run_history.clone();
            let result_tx = result_tx.clone();
            let persist_path = persist_path.clone();
            let id = id.clone();
            let kind = kind.clone();
            Box::pin(async move {
                // Get label for the event
                let label = {
                    let guard = tasks.lock().await;
                    guard
                        .get(&id)
                        .map(|e| e.persisted.label.clone())
                        .unwrap_or_default()
                };

                // Mark running
                {
                    let mut guard = tasks.lock().await;
                    if let Some(entry) = guard.get_mut(&id) {
                        entry.currently_running = true;
                    }
                }
                Self::stamp_fire(&tasks, &persist_path, &id).await;

                // Record run start
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

                // Execute
                let result = executor.execute(&id, &kind).await;
                let duration_ms = start.elapsed().as_millis() as u64;

                // Record run finish + broadcast event
                let (status, result_text, error_text) = match &result {
                    Ok(text) => {
                        run_history
                            .record_finish(&run_id, RunStatus::Completed, Some(text.clone()), None)
                            .await;
                        (RunStatus::Completed, Some(text.clone()), None)
                    }
                    Err(e) => {
                        tracing::error!("Scheduled task {id} failed: {e}");
                        run_history
                            .record_finish(&run_id, RunStatus::Failed, None, Some(e.to_string()))
                            .await;
                        (RunStatus::Failed, None, Some(e.to_string()))
                    }
                };

                // Broadcast result event (for SSE / desktop notifications)
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
            })
        })
        .map_err(|e| anyhow::anyhow!("invalid cron expression '{cron}': {e}"))?;

        let job_id = self.scheduler.add(job).await?;
        Ok(job_id)
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

/// Compute the next fire time for a cron expression from now.
///
/// Returns `None` if the cron expression is invalid or no upcoming occurrence
/// can be found within a reasonable search window.
///
/// Note: `_timezone` is accepted for future use but computation is done in UTC.
/// The cron expression is evaluated against UTC; the scheduler job itself
/// handles timezone-correct firing via `tokio-cron-scheduler`.
fn compute_next_run(cron_expr: &str, _timezone: &str) -> Option<chrono::DateTime<Utc>> {
    // tokio-cron-scheduler uses 6-field cron (sec min hour dom month dow), and
    // both 5- and 6-field expressions have to parse. croner 3 makes that the
    // default — `Seconds::Optional` — so the explicit `.with_seconds_optional()`
    // builder that 2.x needed is gone rather than merely renamed.
    //
    // This must stay on the same croner MAJOR as the one inside
    // `tokio-cron-scheduler`: that copy decides when the job actually fires,
    // this one decides the `next_run` the UI promises. They were 2.x and 3.x.
    let cron: croner::Cron = cron_expr.parse().ok()?;
    let now = Utc::now();
    match cron.find_next_occurrence(&now, false) {
        Ok(dt) => Some(dt),
        Err(e) => {
            tracing::debug!(
                cron_expr,
                error = %e,
                "Failed to compute next_run for cron expression"
            );
            None
        }
    }
}

/// Does this kind's fire stamp have to survive a restart?
///
/// Only a sensor rule reads its own last fire back: the rules engine debounces
/// on it, so a lost stamp re-fires the rule the moment the pond comes back, and
/// a pond that crash-loops fires it every time. That read-back is also what
/// bounds the write — a rule cannot be stamped more often than once per
/// `cooldown_secs`, because the cooldown is the thing the stamp enforces.
///
/// Everything else stays in memory. A cron task's next fire is computed from
/// its expression rather than from its last fire, so persisting the stamp buys
/// nothing, and a 6-field expression is allowed to fire every second — which on
/// the Jetson's flash would be a whole-file rewrite per second. A rule with
/// `cooldown_secs == 0` is excluded for the same reason: it debounces nothing,
/// so it would write on every matching event.
fn durable_fire_stamp(kind: &TaskKind) -> bool {
    matches!(kind, TaskKind::SensorTrigger(spec) if spec.cooldown_secs > 0)
}

/// Write the task list, atomically.
///
/// Write-then-rename rather than truncate-then-write: `rename(2)` within a
/// directory is atomic, so a crash can lose the newest stamp but can never
/// leave a half-written `schedules.json` — which would lose every rule in the
/// pond, not just a cooldown. Fire stamps make this file hot, so the window
/// that used to be opened only by a create or a pause is now opened on every
/// rule fire.
async fn write_snapshot(persist_path: &std::path::Path, records: &[PersistedTask]) -> Result<()> {
    let json = serde_json::to_string_pretty(records)?;
    if let Some(parent) = persist_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = persist_path.with_extension("json.tmp");
    tokio::fs::write(&tmp, json).await?;
    tokio::fs::rename(&tmp, persist_path).await?;
    Ok(())
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
            kind: Some(req.kind.clone()),
            payload: None,
            paused: false,
            created_at: Some(Utc::now()),
            last_run: None,
        };

        // Event-triggered rules (#92) never register a cron job — the rules
        // engine fires them via `run_now` when a matching bus event arrives.
        // The nil job id marks "no cron job", same as the paused state.
        let job_id = if req.kind.is_event_triggered() {
            uuid::Uuid::nil()
        } else {
            self.add_job_to_scheduler(&req.id, &req.cron, req.kind.clone())
                .await?
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
        let (cron, kind) = {
            let guard = self.tasks.lock().await;
            let entry = guard
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            if !entry.persisted.paused {
                return Ok(());
            }
            (entry.persisted.cron.clone(), Self::resolve_kind(entry))
        };

        // Event-triggered rules have no cron job to re-register (#92);
        // clearing `paused` is enough — the rules engine checks the flag.
        let job_id = if kind.is_event_triggered() {
            uuid::Uuid::nil()
        } else {
            self.add_job_to_scheduler(id, &cron, kind).await?
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
        let persist_path = self.persist_path.clone();
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
            Self::stamp_fire(&tasks, &persist_path, &id).await;

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
        let cron_changed = req.cron.is_some();

        // Read current state and apply non-cron changes first.
        let (old_job_id, new_cron, new_kind, was_paused) = {
            let mut guard = self.tasks.lock().await;
            let entry = guard
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;

            if let Some(label) = &req.label {
                entry.persisted.label = label.clone();
            }
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
            let kind = Self::resolve_kind(entry);
            (old_job_id, cron, kind, paused)
        };

        // If the cron changed and the schedule is active, reschedule the job.
        // Create the new job FIRST — if it fails (e.g. invalid cron), the old job
        // stays active and the schedule keeps running with the previous cron.
        if cron_changed && !was_paused {
            let new_job_id = self.add_job_to_scheduler(id, &new_cron, new_kind).await?;
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
