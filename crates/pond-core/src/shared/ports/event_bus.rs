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
use crate::shared::domain::session_activity::SessionLifecycle;
use crate::shared::domain::time_tick::TimeTick;
use crate::user_data::domain::device::{DeviceStateChanged, DeviceStateValue};
use crate::user_data::domain::schedule::{TriggerEventView, TriggerSourceKind};
use crate::user_data::domain::sensor::{CameraEvent, SensorReading};

/// A typed reactive event carried on the bus. A closed enum (rather than the
/// generic [`Event`]) so consumers like the rules engine can pattern-match
/// ergonomically instead of parsing an attribute map.
///
/// The first three variants are *device-shaped*: something in the house
/// reported a reading. The last two are not — they are the pond noticing time
/// passing and the user arriving or going quiet (PAI-7 P1). That split is what
/// [`trigger_view`](BusEvent::trigger_view) returns an `Option` for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum BusEvent {
    Sensor(SensorReading),
    Camera(CameraEvent),
    Device(DeviceStateChanged),
    /// A wall-clock boundary the pond crossed (PAI-7 P1).
    Time(TimeTick),
    /// The user's interaction started, went idle, or resumed (PAI-7 P1).
    Session(SessionLifecycle),
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
            // A clock reading is about nobody, so it is `Public` — the one
            // classification in this match that is not behavioral.
            BusEvent::Time(t) => Event::new(EventCategory::System, "time.tick")
                .attr("boundary", t.boundary.as_str())
                .attr("local_hour", i64::from(t.local_hour))
                .sensitivity(PrivacySensitivity::Public),
            // When somebody is talking to the pond is behavioral data, exactly
            // like a motion reading: `Sensitive`, which both keeps it out of
            // the audit MCP reads and shortens its retention.
            BusEvent::Session(s) => {
                let event = Event::new(EventCategory::Agent, "session.lifecycle")
                    .attr("phase", s.phase.as_str())
                    .attr("idle_secs", i64::try_from(s.idle_secs).unwrap_or(i64::MAX))
                    .sensitivity(PrivacySensitivity::Sensitive);
                match &s.session_id {
                    Some(id) => event.session(id.as_str()),
                    None => event,
                }
            }
        }
    }

    /// Project this bus event onto the fields sensor-trigger rules evaluate
    /// (#92): source family, id, signal name, and an optional numeric value
    /// (sensor value / camera confidence / numeric device state).
    ///
    /// `None` for the events that are not device-shaped. A time tick and a
    /// session transition have no device, no signal and no value, so there is
    /// nothing for a `SensorTrigger` rule to compare against.
    ///
    /// **Returning `None` rather than a placeholder view is the safety
    /// property**, not a stylistic preference: every `TriggerSourceKind` is a
    /// family a user rule may listen to with `device_id: None, signal: None`,
    /// which matches *any* event in that family. A synthetic view would
    /// therefore fire real automations — lights, notifications — on the hour,
    /// every hour, for rules the user wrote about their house.
    ///
    /// The consumer cannot route around the `None` by inventing one, and that
    /// is enforced by the compiler rather than by a test: [`TriggerEventView`]
    /// is `#[non_exhaustive]`, so the line the rules engine would have to
    /// write does not compile in the rules engine's crate.
    ///
    /// ```compile_fail,E0639
    /// use pond_core::user_data::domain::schedule::{TriggerEventView, TriggerSourceKind};
    ///
    /// // What answering `None` with a placeholder looks like. Outside
    /// // pond-core this is a compile error, which is the point.
    /// let placeholder = TriggerEventView {
    ///     kind: TriggerSourceKind::Sensor,
    ///     device_id: "",
    ///     signal: "",
    ///     value: None,
    /// };
    /// ```
    ///
    /// Vacuity control for the block above — a `compile_fail` example also
    /// "passes" when the paths in it are wrong, so here are the same paths in
    /// an example that must compile:
    ///
    /// ```
    /// use pond_core::user_data::domain::schedule::{TriggerEventView, TriggerSourceKind};
    ///
    /// // Reading a view built by `trigger_view` is unaffected: only
    /// // construction is closed.
    /// fn family(view: &TriggerEventView<'_>) -> TriggerSourceKind {
    ///     view.kind
    /// }
    /// ```
    pub fn trigger_view(&self) -> Option<TriggerEventView<'_>> {
        match self {
            BusEvent::Sensor(r) => Some(TriggerEventView {
                kind: TriggerSourceKind::Sensor,
                device_id: &r.device_id,
                signal: &r.sensor_type,
                value: Some(r.value),
            }),
            BusEvent::Camera(c) => Some(TriggerEventView {
                kind: TriggerSourceKind::Camera,
                device_id: &c.camera_id,
                signal: &c.event_type,
                value: c.confidence,
            }),
            BusEvent::Device(d) => Some(TriggerEventView {
                kind: TriggerSourceKind::Device,
                device_id: d.device_id.as_str(),
                signal: &d.key,
                value: match &d.value {
                    DeviceStateValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                    DeviceStateValue::Int(i) => Some(*i as f64),
                    DeviceStateValue::Float(f) => Some(*f),
                    DeviceStateValue::Text(_) => None,
                },
            }),
            BusEvent::Time(_) | BusEvent::Session(_) => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::session_activity::{SessionLifecycle, SessionPhase};
    use crate::shared::domain::time_tick::{TimeBoundary, TimeTick};
    use crate::user_data::domain::schedule::{
        SensorTriggerSpec, TriggerAction, TriggerCondition, TriggerSource,
    };

    fn noon() -> chrono::NaiveTime {
        chrono::NaiveTime::from_hms_opt(12, 0, 0).expect("valid time")
    }

    fn time_tick() -> BusEvent {
        BusEvent::Time(TimeTick {
            boundary: TimeBoundary::Hour,
            at: chrono::Utc::now(),
            local_hour: 12,
        })
    }

    fn session_event(phase: SessionPhase, session_id: Option<&str>) -> BusEvent {
        BusEvent::Session(SessionLifecycle {
            phase,
            session_id: session_id.map(str::to_string),
            at: chrono::Utc::now(),
            idle_secs: 900,
        })
    }

    fn motion_reading() -> BusEvent {
        BusEvent::Sensor(SensorReading {
            device_id: "backyard-pir".into(),
            sensor_type: "motion".into(),
            value: 1.0,
            unit: "bool".into(),
            recorded_at: chrono::Utc::now(),
        })
    }

    fn camera_motion() -> BusEvent {
        BusEvent::Camera(CameraEvent {
            id: None,
            camera_id: "backyard-cam".into(),
            event_type: "motion".into(),
            confidence: Some(0.9),
            snapshot_path: None,
            metadata: None,
            acknowledged: false,
            created_at: chrono::Utc::now(),
        })
    }

    fn device_switched_on() -> BusEvent {
        BusEvent::Device(DeviceStateChanged {
            device_id: "kitchen-lamp".into(),
            key: "power".into(),
            value: DeviceStateValue::Bool(true),
            changed_at: chrono::Utc::now(),
        })
    }

    /// The widest rule the API will accept: a family with no device filter, no
    /// signal filter, no condition. It matches every event in its family, so
    /// it is the rule a clock-driven event would fire if these events were
    /// ever given a device-shaped view.
    fn catch_all_rule(kind: TriggerSourceKind) -> SensorTriggerSpec {
        SensorTriggerSpec {
            source: TriggerSource {
                kind,
                device_id: None,
                signal: None,
            },
            condition: TriggerCondition::default(),
            actions: vec![TriggerAction::Notify {
                title: "t".into(),
                body: "b".into(),
            }],
            cooldown_secs: 0,
        }
    }

    /// Half the point of this test, and the reason it is not vacuous: in every
    /// family there really is a rule the API accepts that fires on *anything*
    /// in that family. That is what a placeholder view for a clock event would
    /// be handed to.
    #[test]
    fn a_catch_all_rule_fires_on_anything_in_its_family() {
        for (kind, event) in [
            (TriggerSourceKind::Sensor, motion_reading()),
            (TriggerSourceKind::Camera, camera_motion()),
            (TriggerSourceKind::Device, device_switched_on()),
        ] {
            let view = event
                .trigger_view()
                .expect("this event is device-shaped and must have a view");
            assert!(
                catch_all_rule(kind).matches(&view, noon()),
                "a {kind:?} rule with no device or signal filter must fire on {event:?}"
            );
        }
    }

    /// PAI-7 P1's whole scope discipline: the new events are published, and no
    /// rule the user wrote can be fired by them — because there is nothing for
    /// a rule to match against at all.
    ///
    /// Read this together with the test above. Alone, "has no view" is a claim
    /// about a function; with it, it is the claim that an hourly tick cannot
    /// run the automations somebody wrote about their house.
    #[test]
    fn a_clock_or_session_event_has_no_view_for_a_rule_to_match() {
        for event in [
            time_tick(),
            session_event(SessionPhase::Started, Some("s-1")),
            session_event(SessionPhase::Idle, None),
            session_event(SessionPhase::Resumed, None),
        ] {
            if let Some(view) = event.trigger_view() {
                panic!(
                    "{event:?} produced the device-shaped view {view:?} — a catch-all rule in \
                     the {:?} family fires on anything, so this would run the user's automations \
                     on every tick",
                    view.kind
                );
            }
        }
    }

    #[test]
    fn a_time_tick_logs_as_a_public_system_event() {
        let event = time_tick().to_event();
        assert_eq!(event.category, EventCategory::System);
        assert_eq!(event.action, "time.tick");
        assert_eq!(event.privacy_sensitivity, PrivacySensitivity::Public);
        assert_eq!(
            event.attributes.get("local_hour"),
            Some(&crate::security::domain::event::AttributeValue::Int(12))
        );
        assert_eq!(
            event.attributes.get("boundary"),
            Some(&crate::security::domain::event::AttributeValue::Text(
                "hour".into()
            ))
        );
    }

    /// When somebody is at the pond is behavioral data. Classifying it below
    /// `Sensitive` would lengthen its retention and expose it to the audit MCP
    /// reads, which exclude sensitive rows.
    #[test]
    fn a_session_transition_is_classified_sensitive_and_carries_its_session() {
        let event = session_event(SessionPhase::Started, Some("sess-42")).to_event();
        assert_eq!(event.category, EventCategory::Agent);
        assert_eq!(event.action, "session.lifecycle");
        assert_eq!(
            event.privacy_sensitivity,
            PrivacySensitivity::Sensitive,
            "when somebody is at the pond is behavioral data; classifying it below Sensitive \
             lengthens its retention and exposes it to the audit MCP reads, which exclude \
             sensitive rows"
        );
        assert_eq!(event.session_id.as_deref(), Some("sess-42"));
        assert_eq!(
            event.attributes.get("phase"),
            Some(&crate::security::domain::event::AttributeValue::Text(
                "started".into()
            ))
        );

        // An activity-clock transition belongs to no session, and must not
        // borrow one.
        let idle = session_event(SessionPhase::Idle, None).to_event();
        assert_eq!(idle.session_id, None);
        assert_eq!(idle.privacy_sensitivity, PrivacySensitivity::Sensitive);
    }

    /// The bus is serialised (it crosses a broadcast channel and is the shape
    /// the event log records), so the new variants must round-trip.
    ///
    /// **Compared whole, not by discriminant.** The first version of this test
    /// asserted only that a `Time` came back a `Time`, which is true of a
    /// payload with every field dropped — and `session_id` carries
    /// `skip_serializing_if`, so silently dropping a field is exactly the
    /// mistake available here. Comparing the payload also means a field added
    /// to either struct tomorrow is covered without anyone remembering to
    /// extend this.
    #[test]
    fn the_new_variants_round_trip_with_their_payloads_intact() {
        let tick = TimeTick {
            boundary: TimeBoundary::Hour,
            at: chrono::Utc::now(),
            local_hour: 12,
        };
        match round_trip(BusEvent::Time(tick.clone())) {
            BusEvent::Time(back) => assert_eq!(back, tick, "the tick lost a field in transit"),
            other => panic!("a Time came back as {other:?}"),
        }

        // Both shapes of `session_id`: the one that serialises and the one
        // `skip_serializing_if` omits, which must come back `None` rather
        // than failing to deserialise for want of the field.
        for lifecycle in [
            SessionLifecycle {
                phase: SessionPhase::Started,
                session_id: Some("sess-42".into()),
                at: chrono::Utc::now(),
                idle_secs: 0,
            },
            SessionLifecycle {
                phase: SessionPhase::Resumed,
                session_id: None,
                at: chrono::Utc::now(),
                idle_secs: 1_800,
            },
        ] {
            match round_trip(BusEvent::Session(lifecycle.clone())) {
                BusEvent::Session(back) => assert_eq!(
                    back, lifecycle,
                    "the session transition lost a field in transit"
                ),
                other => panic!("a Session came back as {other:?}"),
            }
        }
    }

    /// Vacuity control for the round-trip above: an event whose payload is
    /// altered does *not* compare equal, so the assertions there are about the
    /// wire format and not about a `PartialEq` that answers `true` to
    /// everything.
    #[test]
    fn the_round_trip_comparison_can_actually_fail() {
        let at = chrono::Utc::now();
        let tick = TimeTick {
            boundary: TimeBoundary::Hour,
            at,
            local_hour: 12,
        };
        assert_ne!(
            tick,
            TimeTick {
                local_hour: 13,
                ..tick.clone()
            },
            "a tick that lost its hour must not compare equal to one that kept it"
        );

        let started = SessionLifecycle {
            phase: SessionPhase::Started,
            session_id: Some("sess-42".into()),
            at,
            idle_secs: 0,
        };
        assert_ne!(
            started,
            SessionLifecycle {
                session_id: None,
                ..started.clone()
            },
            "a transition that lost its session id must not compare equal to one that kept it"
        );
    }

    fn round_trip(event: BusEvent) -> BusEvent {
        let json = serde_json::to_string(&event).expect("a bus event serialises");
        serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("a bus event must deserialise from {json}: {e}"))
    }
}
