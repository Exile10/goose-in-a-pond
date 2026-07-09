//! Driven Port: in-process event bus (#91).
//!
//! Publish/subscribe for the reactive domain events the Core produces — sensor
//! readings, camera events, and device-state changes — so anything (the rules
//! engine in Q2-15, live dashboards, …) can react without `record_*` knowing
//! its consumers.
//!
//! The port surface is deliberately framework-free: subscription is a
//! `futures::Stream`, never a `tokio::sync::broadcast::Receiver`, so the Core
//! and its consumers don't couple to the broadcast implementation. The
//! in-process tokio-broadcast adapter lives in
//! `crate::shared::services::in_process_event_bus`.

use std::pin::Pin;

use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use crate::user_data::domain::device::{DeviceStateChanged, DeviceStateValue};
use crate::user_data::domain::schedule::{TriggerEventView, TriggerSourceKind};
use crate::user_data::domain::sensor::{CameraEvent, SensorReading};

/// A typed reactive event carried on the bus. A closed enum (rather than the
/// generic [`Event`]) so consumers like the rules engine can pattern-match
/// ergonomically instead of parsing an attribute map.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum BusEvent {
    Sensor(SensorReading),
    Camera(CameraEvent),
    Device(DeviceStateChanged),
}

impl BusEvent {
    /// Project this bus event onto the unified observability [`Event`] so bus
    /// traffic can also be appended to the durable event log (#108). Sensor and
    /// camera data are behavioral, so they're classified `Sensitive`.
    pub fn to_event(&self) -> Event {
        match self {
            BusEvent::Sensor(r) => Event::new(EventCategory::Sensor, "sensor.reading")
                .attr("device_id", r.device_id.as_str())
                .attr("sensor_type", r.sensor_type.as_str())
                .attr("value", r.value)
                .attr("unit", r.unit.as_str())
                .sensitivity(PrivacySensitivity::Sensitive),
            BusEvent::Camera(c) => Event::new(EventCategory::Camera, "camera.event")
                .attr("camera_id", c.camera_id.as_str())
                .attr("event_type", c.event_type.as_str())
                .sensitivity(PrivacySensitivity::Sensitive),
            BusEvent::Device(d) => Event::new(EventCategory::Device, "device.state_changed")
                .attr("device_id", d.device_id.as_str())
                .attr("key", d.key.as_str()),
        }
    }

    /// Project this bus event onto the fields sensor-trigger rules evaluate
    /// (#92): source family, id, signal name, and an optional numeric value
    /// (sensor value / camera confidence / numeric device state).
    pub fn trigger_view(&self) -> TriggerEventView<'_> {
        match self {
            BusEvent::Sensor(r) => TriggerEventView {
                kind: TriggerSourceKind::Sensor,
                device_id: &r.device_id,
                signal: &r.sensor_type,
                value: Some(r.value),
            },
            BusEvent::Camera(c) => TriggerEventView {
                kind: TriggerSourceKind::Camera,
                device_id: &c.camera_id,
                signal: &c.event_type,
                value: c.confidence,
            },
            BusEvent::Device(d) => TriggerEventView {
                kind: TriggerSourceKind::Device,
                device_id: d.device_id.as_str(),
                signal: &d.key,
                value: match &d.value {
                    DeviceStateValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                    DeviceStateValue::Int(i) => Some(*i as f64),
                    DeviceStateValue::Float(f) => Some(*f),
                    DeviceStateValue::Text(_) => None,
                },
            },
        }
    }
}

/// A subscription stream of bus events. Ends when the bus is dropped.
pub type BusStream = Pin<Box<dyn Stream<Item = BusEvent> + Send>>;

/// Driven Port: in-process publish/subscribe for reactive domain events.
pub trait EventBus: Send + Sync {
    /// Publish an event to all current subscribers. Non-blocking and infallible
    /// from the caller's view — having no subscribers is normal, not an error.
    fn publish(&self, event: BusEvent);

    /// Subscribe to events published *after* this call returns.
    fn subscribe(&self) -> BusStream;
}
