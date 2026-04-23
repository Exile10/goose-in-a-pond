//! `CronSchedulerAdapter` — implements `SchedulerPort` via `tokio-cron-scheduler`.
//!
//! Task state (id, label, cron, payload, paused) is persisted to a JSON file
//! so tasks survive process restarts.  The scheduler job map is rebuilt from
//! the persisted state on startup.

use crate::webhook_executor::TaskExecutor;
use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::Utc;
use pond_core::ports::scheduler::{CreateTaskRequest, ScheduledTask, SchedulerPort};
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
    payload: serde_json::Value,
    paused: bool,
}

// ── In-memory state ───────────────────────────────────────────────────────────

struct TaskEntry {
    persisted: PersistedTask,
    job_id: uuid::Uuid,
    currently_running: bool,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct CronSchedulerAdapter {
    scheduler: JobScheduler,
    tasks: Arc<Mutex<HashMap<String, TaskEntry>>>,
    persist_path: PathBuf,
    executor: Arc<dyn TaskExecutor>,
}

impl CronSchedulerAdapter {
    /// Create (or rehydrate) the scheduler.
    ///
    /// `persist_path` — path to the JSON file used to persist task definitions.
    /// `executor`     — called on each job fire.
    pub async fn new(
        persist_path: PathBuf,
        executor: Arc<dyn TaskExecutor>,
    ) -> Result<Self> {
        let scheduler = JobScheduler::new().await?;
        let tasks: Arc<Mutex<HashMap<String, TaskEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let adapter = Self {
            scheduler,
            tasks,
            persist_path,
            executor,
        };

        // Rehydrate persisted tasks
        adapter.rehydrate().await?;

        adapter.scheduler.start().await?;

        Ok(adapter)
    }

    // ── Persistence helpers ───────────────────────────────────────────────────

    async fn save(&self) -> Result<()> {
        let guard = self.tasks.lock().await;
        let records: Vec<PersistedTask> =
            guard.values().map(|e| e.persisted.clone()).collect();
        drop(guard);

        let json = serde_json::to_string_pretty(&records)?;
        if let Some(parent) = self.persist_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.persist_path, json).await?;
        Ok(())
    }

    async fn rehydrate(&self) -> Result<()> {
        if !self.persist_path.exists() {
            return Ok(());
        }

        let json = tokio::fs::read_to_string(&self.persist_path).await?;
        let records: Vec<PersistedTask> = serde_json::from_str(&json)?;

        for record in records {
            // Skip paused tasks — they won't be scheduled but we still keep them
            // in memory so they can be resumed later.
            if record.paused {
                let mut guard = self.tasks.lock().await;
                guard.insert(record.id.clone(), TaskEntry {
                    persisted: record,
                    job_id: uuid::Uuid::nil(),
                    currently_running: false,
                });
                continue;
            }

            let job_id = self
                .add_job_to_scheduler(&record.id, &record.cron, record.payload.clone())
                .await?;

            let mut guard = self.tasks.lock().await;
            guard.insert(record.id.clone(), TaskEntry {
                persisted: record,
                job_id,
                currently_running: false,
            });
        }

        Ok(())
    }

    // ── Internal job management ───────────────────────────────────────────────

    async fn add_job_to_scheduler(
        &self,
        task_id: &str,
        cron: &str,
        payload: serde_json::Value,
    ) -> Result<uuid::Uuid> {
        let executor = self.executor.clone();
        let tasks = self.tasks.clone();
        let id = task_id.to_string();

        let job = Job::new_async(cron, move |_uuid, _lock| {
            let executor = executor.clone();
            let tasks = tasks.clone();
            let id = id.clone();
            let payload = payload.clone();
            Box::pin(async move {
                {
                    let mut guard = tasks.lock().await;
                    if let Some(entry) = guard.get_mut(&id) {
                        entry.currently_running = true;
                    }
                }

                if let Err(e) = executor.execute(&id, payload).await {
                    tracing::error!("Scheduled task {id} failed: {e}");
                }

                {
                    let mut guard = tasks.lock().await;
                    if let Some(entry) = guard.get_mut(&id) {
                        entry.currently_running = false;
                        entry.persisted.paused = false; // ensure not marked paused after run
                    }
                }
            })
        })
        .map_err(|e| anyhow::anyhow!("invalid cron expression '{cron}': {e}"))?;

        let job_id = self.scheduler.add(job).await?;
        Ok(job_id)
    }
}

// ── SchedulerPort impl ────────────────────────────────────────────────────────

#[async_trait]
impl SchedulerPort for CronSchedulerAdapter {
    async fn create_task(&self, req: CreateTaskRequest) -> Result<ScheduledTask> {
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
            payload: req.payload.clone(),
            paused: false,
        };

        let job_id = self
            .add_job_to_scheduler(&req.id, &req.cron, req.payload)
            .await?;

        let task = ScheduledTask {
            id: req.id.clone(),
            label: req.label,
            cron: req.cron,
            last_run: None,
            next_run: None,
            paused: false,
            currently_running: false,
            payload: Some(record.payload.clone()),
        };

        {
            let mut guard = self.tasks.lock().await;
            guard.insert(req.id, TaskEntry {
                persisted: record,
                job_id,
                currently_running: false,
            });
        }

        self.save().await?;

        Ok(task)
    }

    async fn list_tasks(&self) -> Result<Vec<ScheduledTask>> {
        let guard = self.tasks.lock().await;
        let tasks = guard
            .values()
            .map(|e| ScheduledTask {
                id: e.persisted.id.clone(),
                label: e.persisted.label.clone(),
                cron: e.persisted.cron.clone(),
                last_run: None,
                next_run: None,
                paused: e.persisted.paused,
                currently_running: e.currently_running,
                payload: Some(e.persisted.payload.clone()),
            })
            .collect();
        Ok(tasks)
    }

    async fn delete_task(&self, id: &str) -> Result<()> {
        let job_id = {
            let mut guard = self.tasks.lock().await;
            let entry = guard.remove(id).ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            entry.job_id
        };

        // Nil UUID means the job was paused and never added to the scheduler
        if job_id != uuid::Uuid::nil() {
            self.scheduler.remove(&job_id).await?;
        }

        self.save().await?;
        Ok(())
    }

    async fn pause_task(&self, id: &str) -> Result<()> {
        let job_id = {
            let mut guard = self.tasks.lock().await;
            let entry = guard.get_mut(id).ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            if entry.persisted.paused {
                return Ok(()); // already paused
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
        let (cron, payload) = {
            let guard = self.tasks.lock().await;
            let entry = guard.get(id).ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            if !entry.persisted.paused {
                return Ok(()); // not paused
            }
            (entry.persisted.cron.clone(), entry.persisted.payload.clone())
        };

        let job_id = self.add_job_to_scheduler(id, &cron, payload).await?;

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
        let (payload, _paused) = {
            let guard = self.tasks.lock().await;
            let entry = guard.get(id).ok_or_else(|| anyhow::anyhow!("task '{id}' not found"))?;
            (entry.persisted.payload.clone(), entry.persisted.paused)
        };

        let executor = self.executor.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            if let Err(e) = executor.execute(&id, payload).await {
                tracing::error!("run_now task {id} failed: {e}");
            }
        });

        Ok(())
    }
}

// ── Shutdown ──────────────────────────────────────────────────────────────────

impl Drop for CronSchedulerAdapter {
    fn drop(&mut self) {
        // Best-effort shutdown — tokio-cron-scheduler handles cleanup internally.
        let _now = Utc::now(); // suppress unused import warning
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhook_executor::TaskExecutor;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct CountingExecutor(Arc<AtomicU32>);

    #[async_trait]
    impl TaskExecutor for CountingExecutor {
        async fn execute(&self, _id: &str, _payload: serde_json::Value) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    async fn make_scheduler(dir: &std::path::Path) -> CronSchedulerAdapter {
        let counter = Arc::new(AtomicU32::new(0));
        let exec = Arc::new(CountingExecutor(counter));
        CronSchedulerAdapter::new(dir.join("schedules.json"), exec)
            .await
            .expect("scheduler init failed")
    }

    // tokio-cron-scheduler uses 6-field cron: sec min hour dom month dow
    fn create_req(id: &str, cron: &str) -> CreateTaskRequest {
        CreateTaskRequest {
            id: id.to_string(),
            label: format!("Test task {id}"),
            cron: cron.to_string(),
            payload: serde_json::json!({"webhook_url": "http://localhost:9999/hook"}),
        }
    }

    #[tokio::test]
    async fn create_and_list() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched.create_task(create_req("t1", "0 0 0 * * *")).await.unwrap();
        sched.create_task(create_req("t2", "0 0 1 * * *")).await.unwrap();

        let tasks = sched.list_tasks().await.unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().any(|t| t.id == "t1"));
        assert!(tasks.iter().any(|t| t.id == "t2"));
    }

    #[tokio::test]
    async fn delete_task() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;

        sched.create_task(create_req("del", "0 0 2 * * *")).await.unwrap();
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

        sched.create_task(create_req("p1", "0 0 3 * * *")).await.unwrap();
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
            sched.create_task(create_req("persist1", "0 0 4 * * *")).await.unwrap();
        }

        // Rehydrate from disk
        let sched2 = make_scheduler(tmp.path()).await;
        let tasks = sched2.list_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "persist1");
    }

    #[tokio::test]
    async fn duplicate_id_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let sched = make_scheduler(tmp.path()).await;
        sched.create_task(create_req("dup", "0 0 5 * * *")).await.unwrap();
        assert!(sched.create_task(create_req("dup", "0 0 6 * * *")).await.is_err());
    }
}
