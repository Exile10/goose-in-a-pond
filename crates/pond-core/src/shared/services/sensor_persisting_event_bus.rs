//! Publish-side [`EventBus`] decorator that persists sensor readings on their
//! way through the bus (#90).
//!
//! Adapters that produce readings — the Matter bridge today — hold only a bus
//! handle, not a [`SensorStorage`]. They publish `BusEvent::Sensor` so the rules
//! engine and the activity feed react, but nothing wrote those readings to the
//! sensor store, so `giap-sensors` could never answer "what is the temperature
//! in the bedroom?" for a real device. This decorator attaches the write to the
//! handle: whoever is given it gets persistence, whoever holds their own
//! `SensorStorage` does not need it.
//!
//! Two properties a reader needs:
//!
//! - It decorates **publishing only**. `subscribe` delegates straight through,
//!   so subscribers attach to the inner bus and fan-out is unchanged.
//! - It must only be handed to publishers that do *not* persist themselves. The
//!   HTTP `record_sensor` handler writes through `AppState.sensor_storage` and
//!   then publishes, deliberately keeping the plain bus — giving it this one
//!   would write every POSTed reading twice.
//!
//! Ordering matches an undecorated bus: every event, sensor or not, goes through
//! one queue drained by a single task, so nothing is reordered relative to
//! anything else. For a sensor event the record completes before the event is
//! forwarded, which is the same persist-before-publish ordering `record_sensor`
//! gets by writing inline (#91).

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tokio::sync::mpsc;

use crate::shared::ports::event_bus::{BusEvent, BusStream, EventBus};
use crate::user_data::ports::sensor_storage::SensorStorage;

/// Queue depth. The drain does one local SQLite insert per sensor event, which
/// outruns realistic Matter attribute traffic by orders of magnitude, so this
/// only fills while the database is stalled — deep enough to ride out a stall
/// of several seconds without ever holding meaningful memory.
const DEFAULT_QUEUE_CAPACITY: usize = 1024;

pub struct SensorPersistingEventBus {
    tx: mpsc::Sender<BusEvent>,
    inner: Arc<dyn EventBus>,
    /// Latches while the queue is full, so a sustained stall logs once rather
    /// than once per event.
    overloaded: AtomicBool,
}

impl SensorPersistingEventBus {
    pub fn new(inner: Arc<dyn EventBus>, storage: Arc<dyn SensorStorage + Send + Sync>) -> Self {
        Self::with_capacity(inner, storage, DEFAULT_QUEUE_CAPACITY)
    }

    /// Requires a Tokio runtime: the drain task is spawned here rather than
    /// handed back for the caller to spawn, because a forgotten spawn would
    /// silently disable persistence while still delivering every event — the
    /// exact failure this type exists to prevent.
    pub fn with_capacity(
        inner: Arc<dyn EventBus>,
        storage: Arc<dyn SensorStorage + Send + Sync>,
        capacity: usize,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel(capacity);

        let drain_inner = inner.clone();
        tokio::spawn(async move {
            // Sequential by construction: one consumer, one await at a time,
            // so publish order is preserved and each record lands before its
            // event is forwarded. Ends when the last sender drops.
            while let Some(event) = rx.recv().await {
                if let BusEvent::Sensor(reading) = &event {
                    if let Err(e) = storage.record(reading.clone()).await {
                        tracing::warn!(
                            error = %e,
                            device_id = %reading.device_id,
                            sensor_type = %reading.sensor_type,
                            "failed to persist bus sensor reading; forwarding it anyway"
                        );
                    }
                }
                drain_inner.publish(event);
            }
        });

        Self {
            tx,
            inner,
            overloaded: AtomicBool::new(false),
        }
    }
}

impl EventBus for SensorPersistingEventBus {
    fn publish(&self, event: BusEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {
                self.overloaded.store(false, Ordering::Relaxed);
            }
            // Queue full or drain gone. Forward directly rather than drop: a
            // reading that misses the store is a gap in history, but an event
            // that never reaches the bus is a rule that never fires, which
            // would be a regression against the undecorated bus.
            Err(mpsc::error::TrySendError::Full(event))
            | Err(mpsc::error::TrySendError::Closed(event)) => {
                if !self.overloaded.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        "sensor persistence queue is not draining; forwarding events \
                         without storing them until it recovers"
                    );
                }
                self.inner.publish(event);
            }
        }
    }

    fn subscribe(&self) -> BusStream {
        self.inner.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::services::in_process_event_bus::InProcessEventBus;
    use crate::user_data::domain::device::{DeviceStateChanged, DeviceStateValue};
    use crate::user_data::domain::sensor::SensorReading;
    use crate::user_data::mocks::mock_sensor::MockSensorStorage;
    use anyhow::Result;
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use futures::StreamExt;
    use std::time::Duration;
    use tokio::sync::Notify;

    fn reading(value: f64) -> SensorReading {
        SensorReading {
            device_id: "bedroom".into(),
            sensor_type: "temperature".into(),
            value,
            unit: "C".into(),
            recorded_at: Utc::now(),
        }
    }

    /// Wire a decorator over a real in-process bus, returning it alongside the
    /// store so a test can assert on both sides of the seam.
    fn make_bus(
        storage: Arc<dyn SensorStorage + Send + Sync>,
        capacity: usize,
    ) -> (SensorPersistingEventBus, Arc<InProcessEventBus>) {
        let inner = Arc::new(InProcessEventBus::new());
        let bus = SensorPersistingEventBus::with_capacity(inner.clone(), storage, capacity);
        (bus, inner)
    }

    #[tokio::test]
    async fn sensor_event_is_recorded_and_forwarded() {
        let storage = Arc::new(MockSensorStorage::new());
        let (bus, inner) = make_bus(storage.clone(), 8);
        let mut sub = inner.subscribe();

        bus.publish(BusEvent::Sensor(reading(21.5)));

        let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("event within timeout")
            .expect("a bus event");
        assert!(matches!(event, BusEvent::Sensor(_)));

        let stored = storage
            .get_latest("bedroom", "temperature")
            .await
            .unwrap()
            .expect("the reading to have been persisted");
        assert_eq!(stored.value, 21.5);
    }

    #[tokio::test]
    async fn record_completes_before_subscribers_see_the_event() {
        let storage = Arc::new(MockSensorStorage::new());
        let (bus, inner) = make_bus(storage.clone(), 8);
        let mut sub = inner.subscribe();

        bus.publish(BusEvent::Sensor(reading(19.0)));

        tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("event within timeout")
            .expect("a bus event");

        // The #91 invariant: by the time an event is observable, its write is
        // already durable. Deterministic here because the drain awaits the
        // record before forwarding.
        assert!(storage
            .get_latest("bedroom", "temperature")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn non_sensor_events_are_forwarded_without_recording() {
        let storage = Arc::new(MockSensorStorage::new());
        let (bus, inner) = make_bus(storage.clone(), 8);
        let mut sub = inner.subscribe();

        bus.publish(BusEvent::Device(DeviceStateChanged {
            device_id: "lamp".into(),
            key: "on".into(),
            value: DeviceStateValue::Bool(true),
            changed_at: Utc::now(),
        }));

        let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("event within timeout")
            .expect("a bus event");
        assert!(matches!(event, BusEvent::Device(_)));
        assert!(storage.list_sensors().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn publish_order_is_preserved() {
        let storage = Arc::new(MockSensorStorage::new());
        let (bus, inner) = make_bus(storage, 8);
        let mut sub = inner.subscribe();

        for i in 0..5 {
            bus.publish(BusEvent::Sensor(reading(f64::from(i))));
        }

        for expected in 0..5 {
            let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
                .await
                .expect("event within timeout")
                .expect("a bus event");
            match event {
                BusEvent::Sensor(r) => assert_eq!(r.value, f64::from(expected)),
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }

    /// Blocks in `record` until released, so a test can wedge the drain and
    /// fill the queue deterministically.
    struct StallingStorage {
        release: Arc<Notify>,
    }

    #[async_trait]
    impl SensorStorage for StallingStorage {
        async fn record(&self, _reading: SensorReading) -> Result<()> {
            self.release.notified().await;
            Ok(())
        }
        async fn get_latest(&self, _d: &str, _s: &str) -> Result<Option<SensorReading>> {
            Ok(None)
        }
        async fn get_recent(&self, _d: &str, _l: usize) -> Result<Vec<SensorReading>> {
            Ok(vec![])
        }
        async fn get_history(
            &self,
            _d: &str,
            _s: &str,
            _since: Option<DateTime<Utc>>,
            _until: Option<DateTime<Utc>>,
        ) -> Result<Vec<SensorReading>> {
            Ok(vec![])
        }
        async fn list_sensors(&self) -> Result<Vec<(String, String)>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn overflow_forwards_without_persisting() {
        let release = Arc::new(Notify::new());
        let storage = Arc::new(StallingStorage {
            release: release.clone(),
        });
        let (bus, inner) = make_bus(storage, 1);
        let mut sub = inner.subscribe();

        // First is taken by the wedged drain, second fills the queue, third
        // must overflow onto the direct path.
        for i in 0..3 {
            bus.publish(BusEvent::Sensor(reading(f64::from(i))));
        }

        // Delivery is what the overflow path guarantees; ordering is not, since
        // the overflowed event deliberately jumps the stalled queue.
        let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("the overflowed event to be delivered while the drain is stalled")
            .expect("a bus event");
        assert!(matches!(event, BusEvent::Sensor(_)));

        release.notify_waiters();
    }
}
