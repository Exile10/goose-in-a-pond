//! Unified observability event (#108).
//!
//! One append-only event type the whole pipeline emits into, replacing four
//! disjoint silos (`event_log`, `TurnMetrics`, sensors, camera) that had no
//! correlation keys and an untyped `metadata` TEXT blob.
//!
//! Two design choices carry the security/privacy bar:
//! - **Typed [`attributes`]** (not an opaque JSON/TEXT blob) — emitters attach
//!   structured values, so downstream redaction/aggregation never has to parse
//!   arbitrary text.
//! - **[`PrivacySensitivity`]** on every event — emitters must classify the
//!   data, which lets retention/export (Q2-40) drop or mask sensitive events by
//!   policy instead of leaking PII by default.
//!
//! [`attributes`]: Event::attributes
//! Pure domain — no framework imports.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Broad subsystem an event originates from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    Agent,
    Tool,
    Inference,
    Sensor,
    Camera,
    Device,
    Auth,
    Network,
    System,
}

impl EventCategory {
    /// Every variant, for exhaustive iteration (e.g. per-category retention).
    /// Adding a variant breaks this array's length and forces an update.
    pub const ALL: [EventCategory; 9] = [
        EventCategory::Agent,
        EventCategory::Tool,
        EventCategory::Inference,
        EventCategory::Sensor,
        EventCategory::Camera,
        EventCategory::Device,
        EventCategory::Auth,
        EventCategory::Network,
        EventCategory::System,
    ];
}

/// How sensitive an event's contents are. Drives retention and export policy
/// (Q2-40); defaults to the most permissive only where explicitly chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacySensitivity {
    /// Safe to surface anywhere (e.g. "server started").
    Public,
    /// First-party operational data (default for most events).
    Internal,
    /// Personal/behavioral data (who, when, what was said/seen).
    Sensitive,
    /// Credentials/tokens/keys — should generally never be logged at all.
    Secret,
}

/// A single typed attribute value (no opaque text blob).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl From<bool> for AttributeValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}
impl From<i64> for AttributeValue {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}
impl From<f64> for AttributeValue {
    fn from(v: f64) -> Self {
        Self::Float(v)
    }
}
impl From<String> for AttributeValue {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}
impl From<&str> for AttributeValue {
    fn from(v: &str) -> Self {
        Self::Text(v.to_string())
    }
}

/// An append-only, correlatable observability event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub category: EventCategory,
    /// Dotted action name, e.g. `"sensor.reading"`, `"tool.call"`, `"device.set_power"`.
    pub action: String,
    /// Correlates events within one user-facing turn/session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Correlates events across subsystems for one logical operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub attributes: BTreeMap<String, AttributeValue>,
    pub privacy_sensitivity: PrivacySensitivity,
    pub timestamp: DateTime<Utc>,
}

impl Event {
    /// Start a new event for `category`/`action`, stamped `now`, classified
    /// `Internal` by default. Chain `.attr()` / `.session()` / `.sensitivity()`.
    pub fn new(category: EventCategory, action: impl Into<String>) -> Self {
        Self {
            category,
            action: action.into(),
            session_id: None,
            trace_id: None,
            attributes: BTreeMap::new(),
            privacy_sensitivity: PrivacySensitivity::Internal,
            timestamp: Utc::now(),
        }
    }

    pub fn attr(mut self, key: impl Into<String>, value: impl Into<AttributeValue>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    pub fn session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn trace(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    pub fn sensitivity(mut self, level: PrivacySensitivity) -> Self {
        self.privacy_sensitivity = level;
        self
    }
}

/// Filter for querying the event log (all fields optional / AND-combined).
#[derive(Debug, Clone, Default)]
pub struct EventQuery {
    pub category: Option<EventCategory>,
    pub session_id: Option<String>,
    pub trace_id: Option<String>,
    /// Inclusive lower bound on `timestamp`.
    pub since: Option<DateTime<Utc>>,
    /// Exclusive upper bound on `timestamp`.
    pub until: Option<DateTime<Utc>>,
    /// Minimum sensitivity (inclusive): match events whose `privacy_sensitivity`
    /// is `>=` this on the `Public < Internal < Sensitive < Secret` ordering.
    /// Used by sensitivity-aware retention to target sensitive activity.
    pub min_sensitivity: Option<PrivacySensitivity>,
    /// Cap on returned rows (newest first).
    pub limit: Option<usize>,
}

impl EventQuery {
    /// `true` if `event` satisfies every set filter field.
    pub fn matches(&self, event: &Event) -> bool {
        if let Some(c) = self.category {
            if event.category != c {
                return false;
            }
        }
        if let Some(ref s) = self.session_id {
            if event.session_id.as_deref() != Some(s.as_str()) {
                return false;
            }
        }
        if let Some(ref t) = self.trace_id {
            if event.trace_id.as_deref() != Some(t.as_str()) {
                return false;
            }
        }
        if let Some(since) = self.since {
            if event.timestamp < since {
                return false;
            }
        }
        if let Some(until) = self.until {
            if event.timestamp >= until {
                return false;
            }
        }
        if let Some(min) = self.min_sensitivity {
            if event.privacy_sensitivity < min {
                return false;
            }
        }
        true
    }
}
