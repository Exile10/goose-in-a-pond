//! The vision pipeline (#130): frames → motion → (optional classifier) →
//! `CameraEvent` persisted via [`CameraStorage`] and published on the
//! [`EventBus`] — the exact same persist-then-publish contract as the external
//! `POST /api/v1/camera/events` route, so everything downstream (activity API,
//! #92 rules, dashboards) reacts identically to on-device detections.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::user_data::domain::sensor::CameraEvent;
use pond_core::user_data::ports::camera_storage::CameraStorage;
use pond_core::user_data::ports::vision::{FrameSource, VisionClassifier};

use crate::motion::{MotionConfig, MotionDetector};

/// Minimum classifier confidence for a labelled event; below it the event
/// falls back to plain `"motion"`.
const MIN_CLASSIFIER_CONFIDENCE: f64 = 0.5;

#[derive(Debug, Clone)]
pub struct VisionPipelineConfig {
    /// The `camera_id` stamped on emitted events (and matched by #92 rules).
    pub camera_id: String,
    pub motion: MotionConfig,
    /// Minimum interval between emitted events — continuous motion produces
    /// one event per interval, not one per frame.
    pub min_event_interval: Duration,
    /// When set, the triggering frame is saved as a JPEG and the event's
    /// `snapshot_path` points at it (bounded per-camera retention). `None`
    /// keeps the pipeline write-free (tests, RAM-only deployments).
    pub snapshots: Option<crate::snapshot::SnapshotConfig>,
}

impl Default for VisionPipelineConfig {
    fn default() -> Self {
        Self {
            camera_id: "camera-1".to_string(),
            motion: MotionConfig::default(),
            min_event_interval: Duration::from_secs(10),
            snapshots: None,
        }
    }
}

/// Run the pipeline until the frame source ends. Spawn once per camera from
/// `pond-server` startup when vision is enabled.
pub async fn run_vision_pipeline(
    mut source: Box<dyn FrameSource>,
    classifier: Option<Arc<dyn VisionClassifier>>,
    storage: Arc<dyn CameraStorage>,
    bus: Arc<dyn EventBus>,
    cfg: VisionPipelineConfig,
) {
    let mut detector = MotionDetector::new(cfg.motion.clone());
    let mut last_event: Option<Instant> = None;
    tracing::info!(camera = %cfg.camera_id, "vision pipeline started");

    loop {
        let frame = match source.next_frame().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(camera = %cfg.camera_id, error = %e, "frame read failed; stopping");
                break;
            }
        };

        let Some(changed_fraction) = detector.observe(&frame) else {
            continue;
        };
        // Rate-limit: continuous motion → one event per interval.
        if let Some(at) = last_event {
            if at.elapsed() < cfg.min_event_interval {
                continue;
            }
        }

        // Optional enrichment: let the on-device model name what moved.
        let (event_type, confidence) = match &classifier {
            Some(c) => match c.classify(&frame).await {
                Ok(dets) => dets
                    .into_iter()
                    .filter(|d| d.confidence >= MIN_CLASSIFIER_CONFIDENCE)
                    .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
                    .map(|d| (d.label, Some(d.confidence)))
                    .unwrap_or_else(|| ("motion".to_string(), Some(changed_fraction.min(1.0)))),
                Err(e) => {
                    tracing::warn!(camera = %cfg.camera_id, error = %e, "classifier failed; emitting motion");
                    ("motion".to_string(), Some(changed_fraction.min(1.0)))
                }
            },
            None => ("motion".to_string(), Some(changed_fraction.min(1.0))),
        };

        // Best-effort snapshot of the triggering frame: a failed write (full
        // disk, bad mount) must never suppress the event itself.
        let snapshot_path = cfg.snapshots.as_ref().and_then(|snap_cfg| {
            match crate::snapshot::write_snapshot(snap_cfg, &cfg.camera_id, &frame) {
                Ok(path) => Some(path.to_string_lossy().into_owned()),
                Err(e) => {
                    tracing::warn!(camera = %cfg.camera_id, error = %e, "snapshot write failed");
                    None
                }
            }
        });

        let mut event = CameraEvent {
            id: None,
            camera_id: cfg.camera_id.clone(),
            event_type,
            confidence,
            snapshot_path,
            metadata: Some(
                serde_json::json!({ "changed_fraction": changed_fraction, "source": "vision" })
                    .to_string(),
            ),
            acknowledged: false,
            created_at: Utc::now(),
        };

        // Persist first, then publish — same contract as the camera route.
        match storage.record_event(event.clone()).await {
            Ok(id) => {
                event.id = Some(id);
                tracing::info!(
                    camera = %cfg.camera_id,
                    event_type = %event.event_type,
                    changed_fraction,
                    "vision event detected"
                );
                bus.publish(BusEvent::Camera(event));
                last_event = Some(Instant::now());
            }
            Err(e) => {
                tracing::warn!(camera = %cfg.camera_id, error = %e, "failed to persist camera event");
            }
        }
    }
    tracing::info!(camera = %cfg.camera_id, "vision pipeline stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use futures_lite_shim::collect_one_camera_event;
    use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
    use pond_core::user_data::domain::vision::{Detection, Frame};
    use pond_core::user_data::mocks::mock_sensor::MockCameraStorage;
    use std::collections::VecDeque;

    /// Deterministic frame source for tests.
    struct ScriptedFrameSource(VecDeque<Frame>);

    #[async_trait]
    impl FrameSource for ScriptedFrameSource {
        async fn next_frame(&mut self) -> Result<Option<Frame>> {
            Ok(self.0.pop_front())
        }
    }

    /// Always sees a pet (above the confidence floor).
    struct PetClassifier;
    #[async_trait]
    impl VisionClassifier for PetClassifier {
        async fn classify(&self, _frame: &Frame) -> Result<Vec<Detection>> {
            Ok(vec![Detection {
                label: "pet".into(),
                confidence: 0.9,
            }])
        }
    }

    fn flat(luma: u8) -> Frame {
        Frame {
            width: 64,
            height: 36,
            rgb: vec![luma; Frame::expected_len(64, 36)],
            captured_at: Utc::now(),
        }
    }

    fn with_square(background: u8) -> Frame {
        let mut f = flat(background);
        for y in 0..18u32 {
            for x in 0..32u32 {
                let i = ((y * 64 + x) * 3) as usize;
                f.rgb[i] = 255;
                f.rgb[i + 1] = 255;
                f.rgb[i + 2] = 255;
            }
        }
        f
    }

    fn cfg() -> VisionPipelineConfig {
        VisionPipelineConfig {
            camera_id: "backyard-cam".into(),
            motion: MotionConfig::default(),
            min_event_interval: Duration::from_secs(60),
            snapshots: None,
        }
    }

    // Small helper module so the bus subscription read is bounded.
    mod futures_lite_shim {
        use super::*;
        use futures::StreamExt;
        use pond_core::shared::ports::event_bus::BusStream;

        pub async fn collect_one_camera_event(mut stream: BusStream) -> Option<CameraEvent> {
            match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
                Ok(Some(BusEvent::Camera(e))) => Some(e),
                _ => None,
            }
        }
    }

    /// #130 acceptance: a camera frame triggers a detected event that an
    /// automation (#92 rule) can act on — all on-device.
    #[tokio::test]
    async fn frame_motion_persists_publishes_and_matches_a_rule() {
        use pond_core::user_data::domain::schedule::{
            SensorTriggerSpec, TriggerAction, TriggerCondition, TriggerSource, TriggerSourceKind,
        };

        // still, still (baseline), square appears (motion), square again
        // within the rate-limit window (suppressed).
        let frames = VecDeque::from(vec![flat(20), flat(20), with_square(20), flat(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());
        let subscription = bus.subscribe();

        run_vision_pipeline(
            Box::new(ScriptedFrameSource(frames)),
            None,
            storage.clone(),
            bus.clone(),
            cfg(),
        )
        .await;

        // Persisted exactly once (rate limit swallowed the second change).
        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1, "one motion event persisted");
        assert_eq!(stored[0].event_type, "motion");
        assert!(stored[0].confidence.unwrap() > 0.0);

        // Published on the bus…
        let event = collect_one_camera_event(subscription)
            .await
            .expect("camera event on the bus");
        assert_eq!(event.camera_id, "backyard-cam");

        // …and a #92 automation rule matches it (the acceptance chain).
        let rule = SensorTriggerSpec {
            source: TriggerSource {
                kind: TriggerSourceKind::Camera,
                device_id: Some("backyard-cam".into()),
                signal: Some("motion".into()),
            },
            condition: TriggerCondition::default(),
            actions: vec![TriggerAction::Notify {
                title: "Motion".into(),
                body: "Backyard camera motion".into(),
            }],
            cooldown_secs: 60,
        };
        let bus_event = BusEvent::Camera(event);
        let view = bus_event.trigger_view();
        assert!(
            rule.matches(&view, chrono::NaiveTime::from_hms_opt(20, 0, 0).unwrap()),
            "the automation rule must match the on-device detection"
        );
    }

    #[tokio::test]
    async fn classifier_enriches_motion_into_labelled_event() {
        let frames = VecDeque::from(vec![flat(20), with_square(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());

        run_vision_pipeline(
            Box::new(ScriptedFrameSource(frames)),
            Some(Arc::new(PetClassifier)),
            storage.clone(),
            bus,
            cfg(),
        )
        .await;

        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].event_type, "pet", "classifier label used");
        assert_eq!(stored[0].confidence, Some(0.9));
    }

    /// #175 follow-up: with snapshots configured, the emitted event carries a
    /// `snapshot_path` pointing at a real JPEG of the triggering frame.
    #[tokio::test]
    async fn motion_event_carries_a_snapshot_of_the_triggering_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let frames = VecDeque::from(vec![flat(20), with_square(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());
        let mut cfg = cfg();
        cfg.snapshots = Some(crate::snapshot::SnapshotConfig::new(
            tmp.path().to_path_buf(),
        ));

        run_vision_pipeline(
            Box::new(ScriptedFrameSource(frames)),
            None,
            storage.clone(),
            bus,
            cfg,
        )
        .await;

        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        let path = stored[0]
            .snapshot_path
            .as_deref()
            .expect("event must carry a snapshot path");
        assert!(
            std::path::Path::new(path).exists(),
            "snapshot file missing: {path}"
        );
    }

    #[tokio::test]
    async fn still_frames_emit_nothing() {
        let frames = VecDeque::from(vec![flat(20), flat(20), flat(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());

        run_vision_pipeline(
            Box::new(ScriptedFrameSource(frames)),
            None,
            storage.clone(),
            bus,
            cfg(),
        )
        .await;

        assert!(storage
            .list_events("backyard-cam", 10)
            .await
            .unwrap()
            .is_empty());
    }
}
