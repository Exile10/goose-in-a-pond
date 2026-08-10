//! In-process [`EventBus`] backed by a bounded `tokio::sync::broadcast` channel
//! (#91).
//!
//! This is both the production implementation and the test double — it is a
//! pure in-process primitive with no I/O, so it lives in the Core's services
//! rather than an adapter crate.
//!
//! **Bounded by design:** the channel has a fixed capacity, so a slow or stuck
//! subscriber cannot make the bus grow without limit — it simply lags and drops
//! the oldest missed events (surfaced as `Lagged`, which we skip). This caps
//! memory and keeps one bad consumer from stalling publishers.

use async_stream::stream;
use tokio::sync::broadcast;

use crate::shared::ports::event_bus::{BusEvent, BusStream, EventBus};

/// Default channel depth — generous for bursty sensor traffic while bounded.
const DEFAULT_CAPACITY: usize = 256;

/// Broadcast-backed event bus. Cheap to clone via `Arc`; subscribers created
/// after a `publish` do not receive prior events (live pub/sub, not a log).
pub struct InProcessEventBus {
    tx: broadcast::Sender<BusEvent>,
}

impl InProcessEventBus {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Number of live subscribers (useful for diagnostics/tests).
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for InProcessEventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus for InProcessEventBus {
    fn publish(&self, event: BusEvent) {
        // `send` errors only when there are zero subscribers — expected and fine.
        let _ = self.tx.send(event);
    }

    fn subscribe(&self) -> BusStream {
        let mut rx = self.tx.subscribe();
        Box::pin(stream! {
            loop {
                match rx.recv().await {
                    Ok(event) => yield event,
                    // Slow consumer fell behind: skip the gap, keep streaming.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    // All senders dropped: end the stream.
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::sensor::SensorReading;
    use futures::StreamExt;
    use std::sync::Arc;

    fn sample_reading() -> SensorReading {
        SensorReading {
            device_id: "sensor-1".into(),
            sensor_type: "motion".into(),
            value: 1.0,
            unit: "bool".into(),
            recorded_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn subscriber_receives_published_event() {
        let bus = InProcessEventBus::new();
        let mut sub = bus.subscribe();

        bus.publish(BusEvent::Sensor(sample_reading()));

        let received = sub.next().await.expect("a bus event");
        match received {
            BusEvent::Sensor(r) => assert_eq!(r.sensor_type, "motion"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn all_subscribers_receive_each_event() {
        let bus = InProcessEventBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);

        bus.publish(BusEvent::Sensor(sample_reading()));

        assert!(matches!(a.next().await, Some(BusEvent::Sensor(_))));
        assert!(matches!(b.next().await, Some(BusEvent::Sensor(_))));
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_noop() {
        let bus = InProcessEventBus::new();
        bus.publish(BusEvent::Sensor(sample_reading())); // must not panic
        assert_eq!(bus.subscriber_count(), 0);
    }

    /// The bus carries the clock and session-activity events too (PAI-7 P1).
    /// Cheap to assert and worth asserting: the broadcast channel clones every
    /// event to every subscriber, so a variant that is expensive or awkward to
    /// clone is a problem for the whole spine, not just its publisher.
    #[tokio::test]
    async fn subscribers_receive_the_clock_and_session_events() {
        use crate::shared::domain::session_activity::{SessionLifecycle, SessionPhase};
        use crate::shared::domain::time_tick::{TimeBoundary, TimeTick};

        let bus = InProcessEventBus::new();
        let mut sub = bus.subscribe();

        bus.publish(BusEvent::Time(TimeTick {
            boundary: TimeBoundary::Hour,
            at: chrono::Utc::now(),
            local_hour: 7,
        }));
        bus.publish(BusEvent::Session(SessionLifecycle {
            phase: SessionPhase::Idle,
            session_id: None,
            at: chrono::Utc::now(),
            idle_secs: 900,
        }));

        match sub.next().await.expect("a bus event") {
            BusEvent::Time(t) => assert_eq!(t.local_hour, 7),
            other => panic!("unexpected event: {other:?}"),
        }
        match sub.next().await.expect("a bus event") {
            BusEvent::Session(s) => assert_eq!(s.phase, SessionPhase::Idle),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn usable_as_trait_object() {
        let bus: Arc<dyn EventBus> = Arc::new(InProcessEventBus::new());
        let mut sub = bus.subscribe();
        bus.publish(BusEvent::Sensor(sample_reading()));
        assert!(sub.next().await.is_some());
    }
}
