//! Sensor/event-triggered rules engine (#92).
//!
//! Subscribes to the [`EventBus`] stream and, for every sensor/camera/device
//! event, fires any enabled [`TaskKind::SensorTrigger`] rule whose source +
//! condition match — via the scheduler's own `run_now` path, so rule fires get
//! the exact same run records, result broadcasts, and executor dispatch as
//! cron fires.
//!
//! Debounce: each rule carries a `cooldown_secs`; the engine suppresses
//! re-fires inside that window. Time-window conditions ("after sunset" ≈
//! `after "18:30"`) are evaluated against the server's local wall clock.
//!
//! [`EventBus`]: pond_core::shared::ports::event_bus::EventBus

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use pond_core::shared::ports::event_bus::{BusEvent, BusStream};
use pond_core::user_data::domain::schedule::{Schedule, TaskKind};
use pond_core::user_data::ports::scheduler::SchedulerPort;

/// How long a fetched rules list is reused before re-reading from the
/// scheduler. Sensor events can arrive at high frequency; a short TTL keeps
/// per-event overhead flat while newly created rules still take effect within
/// a few seconds.
const RULES_CACHE_TTL: Duration = Duration::from_secs(5);

/// Decide which rules `event` fires right now, stamping their cooldowns.
///
/// Pure over its inputs (clock passed in), so debounce/matching is unit-tested
/// without a scheduler or a bus. A rule fires when it is an enabled
/// `SensorTrigger`, its spec matches the event at `local_time`, and its
/// cooldown window has elapsed.
fn rules_to_fire(
    rules: &[Schedule],
    event: &BusEvent,
    local_time: chrono::NaiveTime,
    now: Instant,
    cooldowns: &mut HashMap<String, Instant>,
) -> Vec<String> {
    let view = event.trigger_view();
    let mut fired = Vec::new();
    for rule in rules {
        let TaskKind::SensorTrigger(spec) = &rule.kind else {
            continue;
        };
        if rule.paused || !spec.matches(&view, local_time) {
            continue;
        }
        if let Some(last) = cooldowns.get(&rule.id) {
            if now.duration_since(*last) < Duration::from_secs(spec.cooldown_secs) {
                continue; // debounced
            }
        }
        cooldowns.insert(rule.id.clone(), now);
        fired.push(rule.id.clone());
    }
    fired
}

/// Run the rules engine until the bus stream ends (server shutdown).
/// Spawn once from `pond-server` startup:
///
/// ```rust,ignore
/// tokio::spawn(pond_infra_scheduler::run_rules_engine(
///     event_bus.subscribe(), scheduler.clone(),
/// ));
/// ```
pub async fn run_rules_engine(mut events: BusStream, scheduler: Arc<dyn SchedulerPort>) {
    let mut cooldowns: HashMap<String, Instant> = HashMap::new();
    let mut cache: Option<(Instant, Vec<Schedule>)> = None;

    while let Some(event) = events.next().await {
        // Refresh the rules snapshot when stale.
        let now = Instant::now();
        let stale = cache
            .as_ref()
            .map(|(at, _)| now.duration_since(*at) >= RULES_CACHE_TTL)
            .unwrap_or(true);
        if stale {
            match scheduler.list_tasks().await {
                Ok(tasks) => cache = Some((now, tasks)),
                Err(e) => {
                    tracing::warn!(error = %e, "rules engine: failed to list rules");
                    // Keep any previous snapshot rather than dropping rules.
                }
            }
        }
        let Some((_, rules)) = &cache else { continue };

        let local_time = chrono::Local::now().time();
        for rule_id in rules_to_fire(rules, &event, local_time, now, &mut cooldowns) {
            tracing::info!(rule = %rule_id, "rules engine: rule matched — firing");
            // `run_now` reuses the scheduler's execution path: run history,
            // Running/Completed result broadcasts, and the executor.
            if let Err(e) = scheduler.run_now(&rule_id).await {
                tracing::warn!(rule = %rule_id, error = %e, "rules engine: fire failed");
            }
        }
    }
    tracing::info!("rules engine: event stream ended");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pond_core::user_data::domain::schedule::{
        SensorTriggerSpec, TriggerAction, TriggerCondition, TriggerSource, TriggerSourceKind,
    };
    use pond_core::user_data::domain::sensor::SensorReading;

    fn motion_event(device: &str) -> BusEvent {
        BusEvent::Sensor(SensorReading {
            device_id: device.into(),
            sensor_type: "motion".into(),
            value: 1.0,
            unit: "bool".into(),
            recorded_at: Utc::now(),
        })
    }

    fn rule(id: &str, device: Option<&str>, cooldown_secs: u64, paused: bool) -> Schedule {
        Schedule {
            id: id.into(),
            label: format!("rule {id}"),
            cron: "@event".into(),
            timezone: "UTC".into(),
            kind: TaskKind::SensorTrigger(SensorTriggerSpec {
                source: TriggerSource {
                    kind: TriggerSourceKind::Sensor,
                    device_id: device.map(String::from),
                    signal: Some("motion".into()),
                },
                condition: TriggerCondition::default(),
                actions: vec![TriggerAction::Notify {
                    title: "t".into(),
                    body: "b".into(),
                }],
                cooldown_secs,
            }),
            paused,
            currently_running: false,
            last_run: None,
            next_run: None,
            created_at: Utc::now(),
        }
    }

    fn noon() -> chrono::NaiveTime {
        chrono::NaiveTime::parse_from_str("12:00", "%H:%M").unwrap()
    }

    #[test]
    fn fires_matching_rule_and_debounces_within_cooldown() {
        let rules = vec![rule("r1", Some("backyard-pir"), 60, false)];
        let mut cooldowns = HashMap::new();
        let now = Instant::now();

        // First event fires.
        let fired = rules_to_fire(
            &rules,
            &motion_event("backyard-pir"),
            noon(),
            now,
            &mut cooldowns,
        );
        assert_eq!(fired, vec!["r1".to_string()]);

        // Second event inside the cooldown is debounced.
        let fired = rules_to_fire(
            &rules,
            &motion_event("backyard-pir"),
            noon(),
            now,
            &mut cooldowns,
        );
        assert!(fired.is_empty(), "cooldown must suppress the re-fire");

        // After the cooldown has elapsed it fires again.
        let later = now + Duration::from_secs(61);
        let fired = rules_to_fire(
            &rules,
            &motion_event("backyard-pir"),
            noon(),
            later,
            &mut cooldowns,
        );
        assert_eq!(fired, vec!["r1".to_string()]);
    }

    #[test]
    fn zero_cooldown_fires_every_event() {
        let rules = vec![rule("r0", None, 0, false)];
        let mut cooldowns = HashMap::new();
        let now = Instant::now();
        assert_eq!(
            rules_to_fire(&rules, &motion_event("a"), noon(), now, &mut cooldowns).len(),
            1
        );
        assert_eq!(
            rules_to_fire(&rules, &motion_event("a"), noon(), now, &mut cooldowns).len(),
            1
        );
    }

    #[test]
    fn skips_paused_and_non_matching_rules() {
        let rules = vec![
            rule("paused", Some("backyard-pir"), 0, true),
            rule("other-device", Some("front-door"), 0, false),
        ];
        let mut cooldowns = HashMap::new();
        let fired = rules_to_fire(
            &rules,
            &motion_event("backyard-pir"),
            noon(),
            Instant::now(),
            &mut cooldowns,
        );
        assert!(fired.is_empty());
    }

    // ── Acceptance (#92): a rule fires end-to-end on a simulated sensor event ─
    //
    // Real CronSchedulerAdapter + a counting executor + the real in-process
    // EventBus: create a SensorTrigger rule, publish a simulated motion
    // reading, and observe the executor invoked via the scheduler's `run_now`
    // path with a Completed run recorded. (The "after sunset" window is
    // covered by the domain tests — a wall-clock-dependent window here would
    // make the test flaky.)
    mod end_to_end {
        use super::*;
        use anyhow::Result;
        use async_trait::async_trait;
        use pond_core::shared::ports::event_bus::EventBus;
        use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
        use pond_core::user_data::domain::schedule::RunStatus;
        use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
        use pond_core::user_data::ports::scheduler::CreateScheduleRequest;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct CountingExecutor(Arc<AtomicU32>);

        #[async_trait]
        impl ScheduleExecutor for CountingExecutor {
            async fn execute(&self, _id: &str, kind: &TaskKind) -> Result<String> {
                assert!(kind.is_event_triggered(), "engine must pass the rule kind");
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok("actions ran".into())
            }
        }

        #[tokio::test]
        async fn sensor_rule_fires_end_to_end_on_simulated_event() {
            let tmp = tempfile::tempdir().unwrap();
            let counter = Arc::new(AtomicU32::new(0));
            let exec: Arc<dyn ScheduleExecutor> = Arc::new(CountingExecutor(counter.clone()));
            let scheduler: Arc<dyn SchedulerPort> = Arc::new(
                crate::CronSchedulerAdapter::new(
                    tmp.path().join("schedules.json"),
                    tmp.path().join("runs.json"),
                    exec,
                )
                .await
                .unwrap(),
            );

            // "Motion on the backyard PIR → notify" (no time window: see above).
            let spec = SensorTriggerSpec {
                source: TriggerSource {
                    kind: TriggerSourceKind::Sensor,
                    device_id: Some("backyard-pir".into()),
                    signal: Some("motion".into()),
                },
                condition: TriggerCondition::default(),
                actions: vec![TriggerAction::Notify {
                    title: "Motion".into(),
                    body: "Backyard motion".into(),
                }],
                cooldown_secs: 60,
            };
            scheduler
                .create_task(CreateScheduleRequest {
                    id: "rule-e2e".into(),
                    label: "Backyard motion".into(),
                    cron: "@event".into(),
                    timezone: "UTC".into(),
                    kind: TaskKind::SensorTrigger(spec),
                })
                .await
                .expect("event rules must not require a valid cron");

            let bus = Arc::new(InProcessEventBus::new());
            tokio::spawn(run_rules_engine(bus.subscribe(), scheduler.clone()));
            // Let the engine subscribe before publishing.
            tokio::time::sleep(Duration::from_millis(50)).await;

            bus.publish(motion_event("backyard-pir"));
            // A second event inside the cooldown must be debounced.
            bus.publish(motion_event("backyard-pir"));

            // Wait (bounded) for the fire to propagate through run_now.
            let mut fired = 0;
            for _ in 0..40 {
                fired = counter.load(Ordering::SeqCst);
                if fired > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert_eq!(fired, 1, "exactly one fire (second event debounced)");

            // The fire went through the scheduler's execution path: a run
            // record exists and completed.
            let mut completed = false;
            for _ in 0..40 {
                let runs = scheduler.get_runs("rule-e2e", 10).await.unwrap();
                if runs.iter().any(|r| {
                    r.status == RunStatus::Completed && r.result.as_deref() == Some("actions ran")
                }) {
                    completed = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(completed, "run record shows the completed rule fire");
        }
    }
}
