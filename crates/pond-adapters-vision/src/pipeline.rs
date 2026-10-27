//! Frames → motion → optional classifier → `CameraEvent`, persisted then published exactly as
//! `POST /api/v1/camera/events` does.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use pond_core::shared::domain::session_activity::{PresenceTransition, ProfilePresence};
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::user_data::domain::sensor::CameraEvent;
use pond_core::user_data::domain::session::IdentificationSource;
use pond_core::user_data::domain::vision::{CapturedFrame, EventSignals, Frame};
use pond_core::user_data::ports::camera_storage::CameraStorage;
use pond_core::user_data::ports::face_recognition::FaceRecognition;
use pond_core::user_data::ports::vision::{FrameSource, VisionClassifier};

use crate::annotate::FrameAnnotation;
use crate::jpeg_decode::{decode_rgb, LumaDecoder};
use crate::live_feed::LiveCameraRegistry;
use crate::motion::{MotionConfig, MotionDetector};

#[derive(Debug, Clone)]
pub struct VisionPipelineConfig {
    /// The `camera_id` on emitted events, which automation rules match on.
    pub camera_id: String,
    pub motion: MotionConfig,
    /// Minimum gap between events; `None` follows the registry's live cooldown setting.
    pub min_event_interval: Option<Duration>,
    /// Save the triggering frame as the event's `snapshot_path` JPEG; `None` writes nothing.
    pub snapshots: Option<crate::snapshot::SnapshotConfig>,
}

impl Default for VisionPipelineConfig {
    fn default() -> Self {
        Self {
            camera_id: "camera-1".to_string(),
            motion: MotionConfig::default(),
            min_event_interval: None,
            snapshots: None,
        }
    }
}

/// Run until the frame source ends (`Ok`) or fails (`Err`); spawn once per camera.
pub async fn run_vision_pipeline(
    source: &mut dyn FrameSource,
    classifier: Option<Arc<dyn VisionClassifier>>,
    face_recognizer: Option<Arc<dyn FaceRecognition>>,
    storage: Arc<dyn CameraStorage>,
    bus: Arc<dyn EventBus>,
    registry: &LiveCameraRegistry,
    cfg: VisionPipelineConfig,
) -> anyhow::Result<()> {
    let mut detector = MotionDetector::new(cfg.motion.clone());
    let mut luma_decoder = LumaDecoder::default();
    let mut last_event = last_recorded_event(storage.as_ref(), &cfg.camera_id).await;
    let mut last_luma_decode: Option<Instant> = None;
    tracing::info!(camera = %cfg.camera_id, "vision pipeline started");

    let ended = loop {
        let captured = match source.next_frame().await {
            Ok(Some(f)) => f,
            Ok(None) => break Ok(()),
            Err(e) => {
                tracing::warn!(camera = %cfg.camera_id, error = %e, "frame read failed; stopping");
                break Err(e);
            }
        };
        let captured_at = captured.captured_at();

        // One JPEG per frame, shared by the live feed, snapshots and face recognition.
        let (jpeg, raw, luma) = match captured {
            CapturedFrame::Raw(frame) => match crate::snapshot::frame_to_jpeg_bytes(&frame) {
                Ok(bytes) => (Some(Arc::new(bytes)), Some(frame), None),
                Err(e) => {
                    tracing::debug!(camera = %cfg.camera_id, error = %e, "frame could not be encoded");
                    (None, Some(frame), None)
                }
            },
            CapturedFrame::Jpeg(j) => (Some(j.jpeg), None, j.luma),
        };
        if let Some(bytes) = &jpeg {
            registry.update_frame(&cfg.camera_id, bytes.clone());
        }

        let bus_cfg = registry.bus_config();
        let min_interval = cfg
            .min_event_interval
            .unwrap_or(Duration::from_secs(bus_cfg.min_event_interval_secs));
        let in_cooldown = last_event.is_some_and(|at| at.elapsed() < min_interval);

        // During cooldown at high FPS, throttle CPU JPEG luma decoding to ~5 Hz.
        if in_cooldown
            && raw.is_none()
            && luma.is_none()
            && last_luma_decode.is_some_and(|t| t.elapsed() < Duration::from_millis(180))
        {
            continue;
        }

        let observed = match (&raw, &luma, &jpeg) {
            (Some(frame), _, _) => detector.observe(frame),
            (None, Some(plane), _) => detector.observe_luma(plane, captured_at),
            (None, None, Some(bytes)) => {
                last_luma_decode = Some(Instant::now());
                match luma_decoder.decode(bytes) {
                    Ok(plane) => detector.observe_luma(plane, captured_at),
                    Err(e) => {
                        tracing::debug!(camera = %cfg.camera_id, error = %e, "camera jpeg could not be decoded");
                        None
                    }
                }
            }
            (None, None, None) => None,
        };
        let Some(changed_fraction) = observed else {
            continue;
        };
        if in_cooldown {
            continue;
        }

        let min_classifier_conf = bus_cfg.classifier_confidence_threshold;

        // Optional enrichment: let the on-device model name what moved.
        let decoded: Option<Frame> = match (&classifier, &raw, &jpeg) {
            (Some(_), None, Some(bytes)) => decode_rgb(bytes, captured_at)
                .map_err(|e| {
                    tracing::warn!(camera = %cfg.camera_id, error = %e, "candidate frame could not be decoded for the classifier");
                })
                .ok(),
            _ => None,
        };
        let classify_started = Instant::now();
        let (mut event_type, confidence) = match (&classifier, raw.as_ref().or(decoded.as_ref())) {
            (Some(c), Some(frame)) => match c.classify(frame).await {
                Ok(dets) => dets
                    .into_iter()
                    .filter(|d| d.confidence >= min_classifier_conf)
                    .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
                    .map(|d| (d.label, Some(d.confidence)))
                    .unwrap_or_else(|| ("motion".to_string(), Some(changed_fraction.min(1.0)))),
                Err(e) => {
                    tracing::warn!(camera = %cfg.camera_id, error = %e, "classifier failed; emitting motion");
                    ("motion".to_string(), Some(changed_fraction.min(1.0)))
                }
            },
            _ => ("motion".to_string(), Some(changed_fraction.min(1.0))),
        };

        let classify_ms = classify_started.elapsed().as_millis() as u64;

        // Face detection and recognition enrichment on person or motion events.
        let face_started = Instant::now();
        let mut identified_profile = None;
        let mut face_confidence = None;
        let mut face_detected = false;
        let mut face_unknown = false;
        let mut face_bbox = None;
        let mut face_landmarks = None;
        let should_check_face = bus_cfg.face_detection_enabled
            && (event_type == "person" || (bus_cfg.face_on_motion && event_type == "motion"));
        if should_check_face {
            if let Some(ref face_rec) = face_recognizer {
                match jpeg.as_deref() {
                    Some(jpeg_bytes) => {
                        match face_rec.identify_with_diagnostics(jpeg_bytes, None).await {
                            Ok(details) => {
                                face_bbox = details.bbox;
                                face_landmarks = details.landmarks;
                                let ident = details.identification;
                                if ident.identified {
                                    tracing::info!(
                                        camera = %cfg.camera_id,
                                        profile = ?ident.profile_id,
                                        confidence = ?ident.confidence,
                                        "face identified in vision event"
                                    );
                                    face_detected = true;
                                    identified_profile = ident.profile_id;
                                    face_confidence = ident.confidence;
                                } else if details.bbox.is_some()
                                    || details.landmarks.is_some()
                                    || ident.confidence.is_some()
                                {
                                    tracing::info!(
                                        camera = %cfg.camera_id,
                                        confidence = ?ident.confidence,
                                        "unrecognized face detected in vision event"
                                    );
                                    face_detected = true;
                                    face_unknown = true;
                                    face_confidence = ident.confidence;
                                }
                                if face_detected && event_type == "motion" {
                                    event_type = "person".to_string();
                                }
                            }
                            Err(e) => {
                                tracing::warn!(camera = %cfg.camera_id, error = %e, "face identification failed");
                            }
                        }
                    }
                    None => {
                        tracing::warn!(camera = %cfg.camera_id, "no encoded frame for face recognition");
                    }
                }
            }
        }

        let face_ms = face_started.elapsed().as_millis() as u64;
        let identified_name = identified_profile
            .as_deref()
            .and_then(|pid| registry.resolve_profile_name(pid));

        let annotation = FrameAnnotation {
            camera_id: cfg.camera_id.clone(),
            event_type: Some(event_type.clone()),
            event_confidence: confidence,
            changed_fraction: Some(changed_fraction),
            face_detected,
            face_unknown,
            identified_profile: identified_profile.clone(),
            identified_name: identified_name.clone(),
            face_confidence,
            face_bbox,
            face_landmarks: face_landmarks.clone(),
            measured_fps: registry.measured_fps(&cfg.camera_id),
            updated_at: Some(captured_at),
        };
        registry.update_annotation(&cfg.camera_id, annotation.clone());

        if !bus_cfg.passes(EventSignals {
            event_type: &event_type,
            face_detected,
            face_recognized: identified_profile.is_some(),
            face_unknown,
        }) {
            continue;
        }

        // After the filter, so a dropped event writes nothing and prunes no kept snapshot.
        // Best-effort: a failed snapshot write must never suppress the event.
        let snapshot_path = cfg.snapshots.as_ref().and_then(|snap_cfg| {
            let bytes = jpeg.as_deref()?;
            match crate::snapshot::write_snapshot_jpeg(snap_cfg, &cfg.camera_id, captured_at, bytes)
            {
                Ok(path) => Some(path.to_string_lossy().into_owned()),
                Err(e) => {
                    tracing::warn!(camera = %cfg.camera_id, error = %e, "snapshot write failed");
                    None
                }
            }
        });

        let mut metadata = serde_json::json!({
            "changed_fraction": changed_fraction,
            "source": "vision",
            "face_detected": face_detected,
            "face_unknown": face_unknown,
        });
        if let Some(ref pid) = identified_profile {
            metadata["identified_profile"] = serde_json::json!(pid);
        }
        if let Some(ref name) = identified_name {
            metadata["identified_name"] = serde_json::json!(name);
        }
        if let Some(conf) = face_confidence {
            metadata["face_confidence"] = serde_json::json!(conf);
        }
        if let Some(bbox) = face_bbox {
            metadata["face_bbox"] = serde_json::json!({
                "x": bbox.x,
                "y": bbox.y,
                "width": bbox.width,
                "height": bbox.height,
            });
        }
        if let Some(ref lm) = face_landmarks {
            metadata["face_landmarks"] = serde_json::json!({
                "left_eye": lm.left_eye,
                "right_eye": lm.right_eye,
                "nose": lm.nose,
                "left_mouth": lm.left_mouth,
                "right_mouth": lm.right_mouth,
            });
        }

        let mut event = CameraEvent {
            id: None,
            camera_id: cfg.camera_id.clone(),
            event_type,
            confidence,
            snapshot_path,
            metadata: Some(metadata.to_string()),
            acknowledged: false,
            created_at: Utc::now(),
        };

        // Persist first, then publish — same contract as the camera route.
        match storage.record_event(event.clone()).await {
            Ok(id) => {
                event.id = Some(id);
                // From the frame's arrival to now: what a household waits for an event.
                let latency_ms = (Utc::now() - captured_at).num_milliseconds();
                tracing::info!(
                    camera = %cfg.camera_id,
                    event_type = %event.event_type,
                    changed_fraction,
                    face_detected,
                    latency_ms,
                    classify_ms,
                    face_ms,
                    "vision event detected"
                );
                registry.publish_event(event.clone());
                bus.publish(BusEvent::Camera(event));
                if let Some(profile_id) = identified_profile {
                    bus.publish(BusEvent::Presence(ProfilePresence {
                        profile_id,
                        transition: PresenceTransition::Arrived,
                        source: IdentificationSource::Face,
                        confidence: face_confidence,
                        session_id: format!("camera:{}", cfg.camera_id),
                        at: Utc::now(),
                    }));
                }
                last_event = Some(Instant::now());
            }
            Err(e) => {
                tracing::warn!(camera = %cfg.camera_id, error = %e, "failed to persist camera event");
            }
        }
    };
    tracing::info!(camera = %cfg.camera_id, "vision pipeline stopped");
    ended
}

/// When this camera's newest stored event happened, so a restart keeps the cooldown it started.
async fn last_recorded_event(storage: &dyn CameraStorage, camera_id: &str) -> Option<Instant> {
    let events = storage.list_events(camera_id, 1).await.ok()?;
    let ago = (Utc::now() - events.first()?.created_at).to_std().ok()?;
    Instant::now().checked_sub(ago)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use futures_lite_shim::collect_one_camera_event;
    use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
    use pond_core::user_data::domain::vision::{Detection, JpegFrame};
    use pond_core::user_data::mocks::mock_sensor::MockCameraStorage;
    use std::collections::VecDeque;

    /// Deterministic frame source for tests.
    struct ScriptedFrameSource(VecDeque<CapturedFrame>);

    impl ScriptedFrameSource {
        fn raw(frames: VecDeque<Frame>) -> Self {
            Self(frames.into_iter().map(CapturedFrame::from).collect())
        }

        /// The same pictures as the camera's own JPEGs, with no decoded luma.
        fn jpeg(frames: VecDeque<Frame>) -> Self {
            Self(
                frames
                    .into_iter()
                    .map(|f| {
                        CapturedFrame::Jpeg(JpegFrame {
                            jpeg: Arc::new(crate::snapshot::frame_to_jpeg_bytes(&f).unwrap()),
                            luma: None,
                            captured_at: f.captured_at,
                        })
                    })
                    .collect(),
            )
        }
    }

    #[async_trait]
    impl FrameSource for ScriptedFrameSource {
        async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
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
            min_event_interval: Some(Duration::from_secs(60)),
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

    #[tokio::test]
    async fn frame_motion_persists_publishes_and_matches_a_rule() {
        use pond_core::user_data::domain::schedule::{
            SensorTriggerSpec, TriggerAction, TriggerCondition, TriggerSource, TriggerSourceKind,
        };

        // Still, still, square appears (motion), square gone within the rate limit (suppressed).
        let frames = VecDeque::from(vec![flat(20), flat(20), with_square(20), flat(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());
        let subscription = bus.subscribe();

        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(frames),
            None,
            None,
            storage.clone(),
            bus.clone(),
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();

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

        // …and an automation rule matches it.
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
        let view = bus_event
            .trigger_view()
            .expect("a camera event is device-shaped");
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
            &mut ScriptedFrameSource::raw(frames),
            Some(Arc::new(PetClassifier)),
            None,
            storage.clone(),
            bus,
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();

        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].event_type, "pet", "classifier label used");
        assert_eq!(stored[0].confidence, Some(0.9));
    }

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
            &mut ScriptedFrameSource::raw(frames),
            None,
            None,
            storage.clone(),
            bus,
            &LiveCameraRegistry::new(),
            cfg,
        )
        .await
        .unwrap();

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
            &mut ScriptedFrameSource::raw(frames),
            None,
            None,
            storage.clone(),
            bus,
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();

        assert!(storage
            .list_events("backyard-cam", 10)
            .await
            .unwrap()
            .is_empty());
    }

    struct PersonClassifier;

    #[async_trait]
    impl VisionClassifier for PersonClassifier {
        async fn classify(&self, _frame: &Frame) -> Result<Vec<Detection>> {
            Ok(vec![Detection {
                label: "person".into(),
                confidence: 0.95,
            }])
        }
    }

    struct MockPersonFaceRecognition {
        profile_id: String,
        confidence: f32,
    }

    #[async_trait]
    impl FaceRecognition for MockPersonFaceRecognition {
        async fn register_face(
            &self,
            _profile_id: &str,
            _image_bytes: &[u8],
            _bbox: Option<pond_core::user_data::domain::face_recognition::BoundingBox>,
        ) -> Result<pond_core::user_data::domain::face_recognition::FaceEmbedding> {
            unimplemented!()
        }

        async fn identify_face(
            &self,
            _image_bytes: &[u8],
            _bbox: Option<pond_core::user_data::domain::face_recognition::BoundingBox>,
        ) -> Result<pond_core::user_data::domain::face_recognition::FaceIdentification> {
            Ok(
                pond_core::user_data::domain::face_recognition::FaceIdentification::found(
                    self.profile_id.clone(),
                    self.confidence,
                ),
            )
        }

        async fn list_embeddings(
            &self,
            _profile_id: &str,
        ) -> Result<Vec<pond_core::user_data::domain::face_recognition::FaceEmbedding>> {
            Ok(vec![])
        }

        async fn delete_embeddings(&self, _profile_id: &str) -> Result<u64> {
            Ok(0)
        }

        async fn pairwise_similarities(
            &self,
        ) -> Result<Vec<pond_core::user_data::ports::face_recognition::PairwiseSimilarity>>
        {
            Ok(vec![])
        }

        fn match_threshold(&self) -> f32 {
            0.7
        }
    }

    #[tokio::test]
    async fn person_classifier_identifies_face_and_emits_presence() {
        use futures::StreamExt;

        let frames = VecDeque::from(vec![flat(20), with_square(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());
        let mut subscription = bus.subscribe();

        let face_rec = Arc::new(MockPersonFaceRecognition {
            profile_id: "jerry".to_string(),
            confidence: 0.88,
        });

        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(frames),
            Some(Arc::new(PersonClassifier)),
            Some(face_rec),
            storage.clone(),
            bus,
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();

        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].event_type, "person");
        let meta: serde_json::Value =
            serde_json::from_str(stored[0].metadata.as_ref().unwrap()).unwrap();
        assert_eq!(meta["identified_profile"], "jerry");
        assert!((meta["face_confidence"].as_f64().unwrap() - 0.88).abs() < 1e-4);

        let mut got_camera = false;
        let mut got_presence = false;
        while let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(500), subscription.next()).await
        {
            match ev {
                BusEvent::Camera(c) => {
                    assert_eq!(c.event_type, "person");
                    got_camera = true;
                }
                BusEvent::Presence(p) => {
                    assert_eq!(p.profile_id, "jerry");
                    assert_eq!(p.transition, PresenceTransition::Arrived);
                    assert_eq!(p.source, IdentificationSource::Face);
                    assert_eq!(p.confidence, Some(0.88));
                    got_presence = true;
                }
                _ => {}
            }
            if got_camera && got_presence {
                break;
            }
        }
        assert!(got_camera, "Camera event was not published");
        assert!(got_presence, "Presence event was not published");
    }

    /// A face recogniser that finds `profile_id`, or an unknown face when it is `None`.
    struct FixedFace(Option<&'static str>);

    #[async_trait]
    impl FaceRecognition for FixedFace {
        async fn register_face(
            &self,
            _profile_id: &str,
            _image_bytes: &[u8],
            _bbox: Option<pond_core::user_data::domain::face_recognition::BoundingBox>,
        ) -> Result<pond_core::user_data::domain::face_recognition::FaceEmbedding> {
            unimplemented!()
        }

        async fn identify_face(
            &self,
            _image_bytes: &[u8],
            _bbox: Option<pond_core::user_data::domain::face_recognition::BoundingBox>,
        ) -> Result<pond_core::user_data::domain::face_recognition::FaceIdentification> {
            use pond_core::user_data::domain::face_recognition::FaceIdentification;
            Ok(match self.0 {
                Some(id) => FaceIdentification::found(id.to_string(), 0.9),
                None => FaceIdentification {
                    identified: false,
                    profile_id: None,
                    confidence: Some(0.2),
                },
            })
        }

        async fn list_embeddings(
            &self,
            _profile_id: &str,
        ) -> Result<Vec<pond_core::user_data::domain::face_recognition::FaceEmbedding>> {
            Ok(vec![])
        }

        async fn delete_embeddings(&self, _profile_id: &str) -> Result<u64> {
            Ok(0)
        }

        async fn pairwise_similarities(
            &self,
        ) -> Result<Vec<pond_core::user_data::ports::face_recognition::PairwiseSimilarity>>
        {
            Ok(vec![])
        }

        fn match_threshold(&self) -> f32 {
            0.7
        }
    }

    fn registry_with(types: &[&str], cooldown_secs: u64) -> LiveCameraRegistry {
        let registry = LiveCameraRegistry::new();
        registry.sync_settings(&pond_core::user_data::domain::settings::Settings {
            vision_event_types: types.iter().map(|t| t.to_string()).collect(),
            vision_event_cooldown_secs: cooldown_secs,
            ..Default::default()
        });
        registry
    }

    /// Two motion detections: the square appears, then goes.
    fn two_motions() -> VecDeque<Frame> {
        VecDeque::from(vec![flat(20), with_square(20), flat(20)])
    }

    async fn run(
        frames: VecDeque<Frame>,
        classifier: Option<Arc<dyn VisionClassifier>>,
        face: Option<Arc<dyn FaceRecognition>>,
        registry: &LiveCameraRegistry,
        cfg: VisionPipelineConfig,
    ) -> Vec<CameraEvent> {
        let storage = Arc::new(MockCameraStorage::new());
        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(frames),
            classifier,
            face,
            storage.clone(),
            Arc::new(InProcessEventBus::new()),
            registry,
            cfg.clone(),
        )
        .await
        .unwrap();
        storage.list_events(&cfg.camera_id, 10).await.unwrap()
    }

    #[tokio::test]
    async fn without_an_override_the_cooldown_is_the_live_setting() {
        let mut cfg = cfg();
        cfg.min_event_interval = None;

        let long = run(
            two_motions(),
            None,
            None,
            &registry_with(&[], 3600),
            cfg.clone(),
        )
        .await;
        assert_eq!(
            long.len(),
            1,
            "a one-hour cooldown swallows the second motion"
        );

        cfg.min_event_interval = Some(Duration::ZERO);
        let overridden = run(two_motions(), None, None, &registry_with(&[], 3600), cfg).await;
        assert_eq!(
            overridden.len(),
            2,
            "an explicit interval wins over the setting, whatever its value"
        );
    }

    #[tokio::test]
    async fn the_default_interval_is_ten_seconds_from_the_setting_not_a_sentinel() {
        // The default config has no override; the setting's default is the ten seconds.
        assert_eq!(VisionPipelineConfig::default().min_event_interval, None);
        let cfg = VisionPipelineConfig {
            camera_id: "backyard-cam".into(),
            min_event_interval: Some(Duration::from_secs(10)),
            ..VisionPipelineConfig::default()
        };
        let events = run(two_motions(), None, None, &registry_with(&[], 1), cfg).await;
        assert_eq!(
            events.len(),
            1,
            "an explicit 10 s is honoured, not read as unset"
        );
    }

    #[tokio::test]
    async fn enabling_only_face_recognized_keeps_a_recognised_person() {
        let registry = registry_with(&["face_recognized"], 10);
        let events = run(
            two_motions(),
            Some(Arc::new(PersonClassifier)),
            Some(Arc::new(FixedFace(Some("jerry")))),
            &registry,
            cfg(),
        )
        .await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "person");
        let meta: serde_json::Value =
            serde_json::from_str(events[0].metadata.as_deref().unwrap()).unwrap();
        assert_eq!(meta["identified_profile"], "jerry");
    }

    #[tokio::test]
    async fn enabling_only_face_unknown_keeps_a_stranger_and_drops_a_household_member() {
        let registry = registry_with(&["face_unknown"], 10);
        let stranger = run(
            two_motions(),
            Some(Arc::new(PersonClassifier)),
            Some(Arc::new(FixedFace(None))),
            &registry,
            cfg(),
        )
        .await;
        assert_eq!(stranger.len(), 1);
        let meta: serde_json::Value =
            serde_json::from_str(stranger[0].metadata.as_deref().unwrap()).unwrap();
        assert_eq!(meta["face_unknown"], true);

        let member = run(
            two_motions(),
            Some(Arc::new(PersonClassifier)),
            Some(Arc::new(FixedFace(Some("jerry")))),
            &registry,
            cfg(),
        )
        .await;
        assert!(member.is_empty(), "a recognised face is not face_unknown");
    }

    #[tokio::test]
    async fn a_filtered_out_event_writes_no_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = cfg();
        cfg.snapshots = Some(crate::snapshot::SnapshotConfig::new(
            tmp.path().to_path_buf(),
        ));
        let events = run(
            two_motions(),
            Some(Arc::new(PetClassifier)),
            None,
            &registry_with(&["person"], 10),
            cfg,
        )
        .await;
        assert!(events.is_empty(), "pet is not an enabled type");
        let written = std::fs::read_dir(tmp.path())
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(
            written, 0,
            "a dropped event must not write or prune snapshots"
        );
    }

    #[tokio::test]
    async fn the_classifier_floor_follows_the_live_setting() {
        // PetClassifier answers 0.9: a 0.95 floor turns it back into plain motion.
        let registry = LiveCameraRegistry::new();
        registry.sync_settings(&pond_core::user_data::domain::settings::Settings {
            vision_classifier_confidence: 0.95,
            ..Default::default()
        });
        let events = run(
            VecDeque::from(vec![flat(20), with_square(20)]),
            Some(Arc::new(PetClassifier)),
            None,
            &registry,
            cfg(),
        )
        .await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "motion");
    }

    /// Records what the classifier was shown.
    struct SeenClassifier(std::sync::Mutex<Vec<(u32, u32)>>);

    #[async_trait]
    impl VisionClassifier for SeenClassifier {
        async fn classify(&self, frame: &Frame) -> Result<Vec<Detection>> {
            self.0.lock().unwrap().push((frame.width, frame.height));
            Ok(vec![Detection {
                label: "pet".into(),
                confidence: 0.9,
            }])
        }
    }

    #[tokio::test]
    async fn camera_jpegs_feed_the_live_view_and_snapshots_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = cfg();
        cfg.snapshots = Some(crate::snapshot::SnapshotConfig::new(
            tmp.path().to_path_buf(),
        ));
        let mut source = ScriptedFrameSource::jpeg(VecDeque::from(vec![flat(20), with_square(20)]));
        let CapturedFrame::Jpeg(last) = source.0.back().unwrap().clone() else {
            unreachable!()
        };
        let classifier = Arc::new(SeenClassifier(Default::default()));
        let storage = Arc::new(MockCameraStorage::new());
        let registry = LiveCameraRegistry::new();

        run_vision_pipeline(
            &mut source,
            Some(classifier.clone()),
            None,
            storage.clone(),
            Arc::new(InProcessEventBus::new()),
            &registry,
            cfg,
        )
        .await
        .unwrap();

        assert_eq!(
            registry.get_latest_frame("backyard-cam").as_deref(),
            Some(last.jpeg.as_slice()),
            "the live view carries the camera's bytes, not a re-encode"
        );
        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1, "motion found by decoding the jpeg");
        assert_eq!(stored[0].event_type, "pet");
        let snapshot = std::fs::read(stored[0].snapshot_path.as_deref().unwrap()).unwrap();
        assert_eq!(
            snapshot, *last.jpeg,
            "the snapshot is the camera's own jpeg"
        );
        assert_eq!(
            *classifier.0.lock().unwrap(),
            vec![(64, 36)],
            "only the one candidate is decoded, at full size"
        );
    }

    #[tokio::test]
    async fn a_decoded_luma_picture_is_used_instead_of_decoding_the_jpeg() {
        // The JPEG is garbage: only the luma that came with it can show the motion.
        let luma = |v: u8, square: bool| {
            let mut pixels = vec![v; 64 * 36];
            if square {
                for y in 0..18 {
                    pixels[y * 64..y * 64 + 32].fill(255);
                }
            }
            pond_core::user_data::domain::vision::LumaPlane {
                width: 64,
                height: 36,
                pixels,
            }
        };
        let frames = [luma(20, false), luma(20, true)]
            .into_iter()
            .map(|l| {
                CapturedFrame::Jpeg(JpegFrame {
                    jpeg: Arc::new(b"not a jpeg".to_vec()),
                    luma: Some(l),
                    captured_at: Utc::now(),
                })
            })
            .collect();
        let storage = Arc::new(MockCameraStorage::new());
        run_vision_pipeline(
            &mut ScriptedFrameSource(frames),
            None,
            None,
            storage.clone(),
            Arc::new(InProcessEventBus::new()),
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();
        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].event_type, "motion",
            "an undecodable candidate stays motion"
        );
    }

    #[tokio::test]
    async fn a_restarted_pipeline_keeps_the_cooldown_the_last_event_started() {
        let storage = Arc::new(MockCameraStorage::new());
        for _ in 0..2 {
            run_vision_pipeline(
                &mut ScriptedFrameSource::raw(VecDeque::from(vec![flat(20), with_square(20)])),
                None,
                None,
                storage.clone(),
                Arc::new(InProcessEventBus::new()),
                &LiveCameraRegistry::new(),
                cfg(),
            )
            .await
            .unwrap();
        }
        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(
            stored.len(),
            1,
            "the second run starts inside the first event's cooldown"
        );
    }

    #[tokio::test]
    async fn a_failed_frame_read_is_reported_as_the_reason_the_pipeline_ended() {
        struct Broken;
        #[async_trait]
        impl FrameSource for Broken {
            async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
                anyhow::bail!("ffmpeg exited")
            }
        }
        let ended = run_vision_pipeline(
            &mut Broken,
            None,
            None,
            Arc::new(MockCameraStorage::new()),
            Arc::new(InProcessEventBus::new()),
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await;
        assert!(ended.unwrap_err().to_string().contains("ffmpeg exited"));
    }
}
