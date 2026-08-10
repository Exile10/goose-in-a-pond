//! Schedule domain types — pure Rust, no external framework imports.
//!
//! Represents scheduled automations that fire on a cron cadence.
//! Each schedule carries a [`TaskKind`] that determines what happens
//! on each fire: send a prompt to the LLM agent, or POST a webhook.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a scheduled task does when it fires.
///
/// `PartialEq` only (not `Eq`): [`SensorTriggerSpec`] carries an `f64`
/// comparison threshold.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskKind {
    /// Send the prompt to the LLM agent and store the response.
    AgentPrompt { prompt: String },
    /// POST to an external webhook URL (backward compat).
    Webhook { webhook_url: String },
    /// Fire when a matching sensor/camera/device event arrives on the
    /// EventBus (#92). Event-triggered: never registered with cron — the
    /// rules engine invokes it via the scheduler's `run_now` path.
    SensorTrigger(SensorTriggerSpec),
}

impl TaskKind {
    /// `true` for kinds that fire on events rather than a cron cadence.
    /// The scheduler skips cron registration for these.
    pub fn is_event_triggered(&self) -> bool {
        matches!(self, TaskKind::SensorTrigger(_))
    }
}

// ── Sensor/event-triggered rules (#92) ───────────────────────────────────────

/// Which bus-event family a rule listens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerSourceKind {
    Sensor,
    Camera,
    Device,
}

/// What the rule listens to. Unset fields match anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerSource {
    pub kind: TriggerSourceKind,
    /// `device_id` / `camera_id` to match; `None` = any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Sensor `sensor_type` / camera `event_type` / device state key to
    /// match (e.g. `"motion"`, `"person"`, `"power"`); `None` = any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

/// Numeric comparison operator for [`TriggerCondition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareOp {
    Gt,
    Gte,
    Lt,
    Lte,
    Eq,
}

/// When a matching event actually fires the rule. All set parts must hold
/// (AND). An empty condition always matches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TriggerCondition {
    /// Numeric comparison against the event's value (sensor reading value,
    /// camera confidence, or numeric device state). Requires `value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<CompareOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// Local-time window bounds, `"HH:MM"`. When `after` > `before` the
    /// window wraps midnight (e.g. `after "18:30"`, `before "06:00"` ≈
    /// "after sunset"). Malformed bounds fail closed (never match).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
}

/// One action a fired rule performs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriggerAction {
    /// Send a prompt to the LLM agent (same path as scheduled prompts).
    AgentPrompt { prompt: String },
    /// Switch a device on/off via the device-control port.
    DevicePower { device_id: String, on: bool },
    /// Push a notification to connected clients.
    Notify { title: String, body: String },
}

/// A sensor/event-triggered automation rule (#92):
/// "if `source` emits an event matching `condition`, run `actions`".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensorTriggerSpec {
    pub source: TriggerSource,
    #[serde(default)]
    pub condition: TriggerCondition,
    pub actions: Vec<TriggerAction>,
    /// Minimum seconds between fires (debounce/cooldown). Default 60.
    #[serde(default = "SensorTriggerSpec::default_cooldown_secs")]
    pub cooldown_secs: u64,
}

/// A bus event projected onto the fields trigger evaluation needs — keeps
/// the evaluation pure and testable without importing the bus type here.
/// Built by `BusEvent::trigger_view()`.
///
/// **`#[non_exhaustive]` is a safety property, not future-proofing** (PAI-7
/// P1). `trigger_view()` returns `None` for the events that are not
/// device-shaped — a clock tick, a session transition — because a rule written
/// with `device_id: None, signal: None` matches *anything* in its family, so a
/// placeholder view would fire the automations somebody wrote about their
/// house on the hour, every hour. This attribute is what stops a consumer in
/// another crate answering that `None` with a view of its own: outside
/// pond-core the struct literal does not compile at all. Its fields stay
/// readable.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TriggerEventView<'a> {
    pub kind: TriggerSourceKind,
    pub device_id: &'a str,
    pub signal: &'a str,
    /// Sensor value / camera confidence / numeric device state, if any.
    pub value: Option<f64>,
}

impl SensorTriggerSpec {
    pub fn default_cooldown_secs() -> u64 {
        60
    }

    /// Pure evaluation: does `event` at local wall-clock `local_time`
    /// satisfy this rule's source + condition?
    pub fn matches(&self, event: &TriggerEventView<'_>, local_time: chrono::NaiveTime) -> bool {
        // Source family + optional exact filters.
        if self.source.kind != event.kind {
            return false;
        }
        if let Some(d) = &self.source.device_id {
            if d != event.device_id {
                return false;
            }
        }
        if let Some(s) = &self.source.signal {
            if s != event.signal {
                return false;
            }
        }

        // Numeric comparison (requires both an operator and a threshold).
        if let (Some(op), Some(threshold)) = (self.condition.op, self.condition.value) {
            let Some(v) = event.value else {
                return false;
            };
            let ok = match op {
                CompareOp::Gt => v > threshold,
                CompareOp::Gte => v >= threshold,
                CompareOp::Lt => v < threshold,
                CompareOp::Lte => v <= threshold,
                CompareOp::Eq => (v - threshold).abs() < f64::EPSILON,
            };
            if !ok {
                return false;
            }
        }

        // Local-time window. Malformed bounds fail closed.
        in_time_window(
            self.condition.after.as_deref(),
            self.condition.before.as_deref(),
            local_time,
        )
    }
}

/// `true` when `t` falls inside the optional `[after, before)` local-time
/// window; wraps midnight when `after` > `before`. Malformed bounds → false.
fn in_time_window(after: Option<&str>, before: Option<&str>, t: chrono::NaiveTime) -> bool {
    let parse = |s: &str| chrono::NaiveTime::parse_from_str(s, "%H:%M").ok();
    match (after, before) {
        (None, None) => true,
        (Some(a), None) => parse(a).map(|a| t >= a).unwrap_or(false),
        (None, Some(b)) => parse(b).map(|b| t < b).unwrap_or(false),
        (Some(a), Some(b)) => match (parse(a), parse(b)) {
            (Some(a), Some(b)) if a <= b => t >= a && t < b,
            (Some(a), Some(b)) => t >= a || t < b, // wraps midnight
            _ => false,
        },
    }
}

/// A scheduled automation persisted by the scheduler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub label: String,
    /// 6-field cron expression: `<sec> <min> <hour> <dom> <month> <dow>`
    pub cron: String,
    /// IANA timezone (e.g. `"Africa/Nairobi"`). Cron is evaluated in this zone.
    pub timezone: String,
    pub kind: TaskKind,
    pub paused: bool,
    pub currently_running: bool,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Status of a single scheduled execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
}

/// A single execution record for a scheduled task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRun {
    pub id: String,
    pub schedule_id: String,
    pub status: RunStatus,
    /// The agent's response text, or webhook status message.
    pub result: Option<String>,
    /// Error message if `status == Failed`.
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
}

/// Event emitted when a scheduled task completes (for SSE broadcast / desktop notification).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleResultEvent {
    pub schedule_id: String,
    pub schedule_label: String,
    pub run_id: String,
    pub status: RunStatus,
    pub result: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_kind_serde_round_trip_agent() {
        let kind = TaskKind::AgentPrompt {
            prompt: "Good morning briefing".into(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"agent_prompt\""));
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn task_kind_serde_round_trip_webhook() {
        let kind = TaskKind::Webhook {
            webhook_url: "https://example.com/hook".into(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"webhook\""));
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn run_status_serde() {
        let s = RunStatus::Completed;
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"completed\"");
        let back: RunStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, RunStatus::Completed);
    }

    // ── Sensor-trigger rules (#92) ────────────────────────────────────────

    fn motion_after_sunset_rule() -> SensorTriggerSpec {
        SensorTriggerSpec {
            source: TriggerSource {
                kind: TriggerSourceKind::Sensor,
                device_id: Some("backyard-pir".into()),
                signal: Some("motion".into()),
            },
            condition: TriggerCondition {
                op: Some(CompareOp::Gte),
                value: Some(1.0),
                after: Some("18:30".into()),
                before: Some("06:00".into()),
            },
            actions: vec![
                TriggerAction::DevicePower {
                    device_id: "backyard-lights".into(),
                    on: true,
                },
                TriggerAction::Notify {
                    title: "Motion".into(),
                    body: "Backyard motion after sunset".into(),
                },
            ],
            cooldown_secs: 120,
        }
    }

    fn motion_event<'a>() -> TriggerEventView<'a> {
        TriggerEventView {
            kind: TriggerSourceKind::Sensor,
            device_id: "backyard-pir",
            signal: "motion",
            value: Some(1.0),
        }
    }

    fn at(hhmm: &str) -> chrono::NaiveTime {
        chrono::NaiveTime::parse_from_str(hhmm, "%H:%M").unwrap()
    }

    #[test]
    fn task_kind_serde_round_trip_sensor_trigger() {
        let kind = TaskKind::SensorTrigger(motion_after_sunset_rule());
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"type\":\"sensor_trigger\""));
        let back: TaskKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
        assert!(back.is_event_triggered());
        assert!(!TaskKind::Webhook {
            webhook_url: "https://x".into()
        }
        .is_event_triggered());
    }

    #[test]
    fn sensor_trigger_defaults_deserialize() {
        // Minimal JSON: no condition, no cooldown → empty condition + 60s.
        let json = r#"{"type":"sensor_trigger","source":{"kind":"sensor"},"actions":[{"type":"notify","title":"t","body":"b"}]}"#;
        let TaskKind::SensorTrigger(spec) = serde_json::from_str::<TaskKind>(json).unwrap() else {
            panic!("wrong kind");
        };
        assert_eq!(spec.cooldown_secs, 60);
        assert_eq!(spec.condition, TriggerCondition::default());
        // Empty condition + no filters matches any sensor event, any time.
        assert!(spec.matches(&motion_event(), at("12:00")));
    }

    #[test]
    fn matches_motion_after_sunset_inside_wrapped_window() {
        let rule = motion_after_sunset_rule();
        // Evening and small hours are inside the 18:30→06:00 wrap…
        assert!(rule.matches(&motion_event(), at("22:15")));
        assert!(rule.matches(&motion_event(), at("05:59")));
        // …midday is outside.
        assert!(!rule.matches(&motion_event(), at("12:00")));
        assert!(!rule.matches(&motion_event(), at("06:00")));
    }

    #[test]
    fn matches_filters_source_and_value() {
        let rule = motion_after_sunset_rule();
        let evening = at("20:00");

        // Wrong device.
        let mut e = motion_event();
        e.device_id = "front-door";
        assert!(!rule.matches(&e, evening));

        // Wrong signal.
        let mut e = motion_event();
        e.signal = "temperature";
        assert!(!rule.matches(&e, evening));

        // Wrong family (camera event, same names).
        let mut e = motion_event();
        e.kind = TriggerSourceKind::Camera;
        assert!(!rule.matches(&e, evening));

        // Value below threshold, and value missing.
        let mut e = motion_event();
        e.value = Some(0.0);
        assert!(!rule.matches(&e, evening));
        e.value = None;
        assert!(!rule.matches(&e, evening));
    }

    #[test]
    fn compare_ops_evaluate_correctly() {
        let mk = |op, threshold| SensorTriggerSpec {
            source: TriggerSource {
                kind: TriggerSourceKind::Sensor,
                device_id: None,
                signal: None,
            },
            condition: TriggerCondition {
                op: Some(op),
                value: Some(threshold),
                after: None,
                before: None,
            },
            actions: vec![],
            cooldown_secs: 0,
        };
        let ev = |v| TriggerEventView {
            kind: TriggerSourceKind::Sensor,
            device_id: "d",
            signal: "s",
            value: Some(v),
        };
        let noon = at("12:00");
        assert!(mk(CompareOp::Gt, 20.0).matches(&ev(21.0), noon));
        assert!(!mk(CompareOp::Gt, 20.0).matches(&ev(20.0), noon));
        assert!(mk(CompareOp::Gte, 20.0).matches(&ev(20.0), noon));
        assert!(mk(CompareOp::Lt, 20.0).matches(&ev(19.9), noon));
        assert!(mk(CompareOp::Lte, 20.0).matches(&ev(20.0), noon));
        assert!(mk(CompareOp::Eq, 1.0).matches(&ev(1.0), noon));
        assert!(!mk(CompareOp::Eq, 1.0).matches(&ev(0.5), noon));
    }

    #[test]
    fn malformed_time_window_fails_closed() {
        let mut rule = motion_after_sunset_rule();
        rule.condition.after = Some("sunset".into()); // not HH:MM
        assert!(!rule.matches(&motion_event(), at("22:00")));
    }

    #[test]
    fn non_wrapping_window_and_half_open_bounds() {
        // 08:00–17:00 plain window.
        assert!(in_time_window(Some("08:00"), Some("17:00"), at("12:00")));
        assert!(!in_time_window(Some("08:00"), Some("17:00"), at("18:00")));
        // Only `after`.
        assert!(in_time_window(Some("18:30"), None, at("23:00")));
        assert!(!in_time_window(Some("18:30"), None, at("06:00")));
        // Only `before`.
        assert!(in_time_window(None, Some("06:00"), at("05:00")));
        assert!(!in_time_window(None, Some("06:00"), at("07:00")));
    }
}
