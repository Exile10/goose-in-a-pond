//! Keeps one capture pipeline matched to the stored settings: starts it, restarts it when a
//! capture setting changes or the stream dies, and stops it when vision or camera consent is off.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use pond_core::shared::ports::event_bus::EventBus;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::domain::vision as policy;
use pond_core::user_data::ports::camera_storage::CameraStorage;
use pond_core::user_data::ports::face_recognition::FaceRecognition;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::vision::{FrameSource, VisionClassifier};

use crate::ffmpeg_source::{CaptureConfig, FfmpegFrameSource};
use crate::live_feed::{LiveCameraRegistry, SourceKind, VisionState};
use crate::motion::MotionConfig;
use crate::pipeline::{run_vision_pipeline, VisionPipelineConfig};
use crate::snapshot::SnapshotConfig;

/// How often settings are re-read; also the longest a revoked consent keeps a camera open.
pub const SETTINGS_POLL_INTERVAL: Duration = Duration::from_secs(2);
pub const RECONNECT_BACKOFF_MIN: Duration = Duration::from_secs(2);
pub const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Opens a frame source for a capture config; injected so tests need no ffmpeg or camera.
pub type SourceOpener =
    Arc<dyn Fn(&CaptureConfig) -> anyhow::Result<Box<dyn FrameSource>> + Send + Sync>;
/// Finds a local camera when none is configured.
pub type CameraDiscovery = Arc<dyn Fn() -> Option<String> + Send + Sync>;

#[derive(Debug, Clone, Copy)]
pub struct SupervisorTiming {
    pub poll: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
}

impl Default for SupervisorTiming {
    fn default() -> Self {
        Self {
            poll: SETTINGS_POLL_INTERVAL,
            backoff_min: RECONNECT_BACKOFF_MIN,
            backoff_max: RECONNECT_BACKOFF_MAX,
        }
    }
}

/// What a running pipeline was started with; a difference in any field means a restart.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureSpec {
    pub source: String,
    pub source_kind: SourceKind,
    pub camera_id: String,
    pub fps: u32,
    pub motion_threshold: f64,
}

impl CaptureSpec {
    fn capture_config(&self) -> CaptureConfig {
        CaptureConfig {
            input: self.source.clone(),
            fps: self.fps,
            ..Default::default()
        }
    }

    fn pipeline_config(&self, snapshots: Option<SnapshotConfig>) -> VisionPipelineConfig {
        VisionPipelineConfig {
            camera_id: self.camera_id.clone(),
            motion: MotionConfig {
                changed_fraction: self.motion_threshold,
                ..Default::default()
            },
            min_event_interval: None,
            snapshots,
        }
    }
}

/// What the settings ask the supervisor to do right now.
#[derive(Debug, Clone, PartialEq)]
pub enum DesiredCapture {
    ConsentOff,
    VisionOff,
    NoCamera,
    Capture(CaptureSpec),
}

impl DesiredCapture {
    fn idle_state(&self) -> Option<VisionState> {
        match self {
            DesiredCapture::ConsentOff => Some(VisionState::ConsentOff),
            DesiredCapture::VisionOff => Some(VisionState::Off),
            DesiredCapture::NoCamera => Some(VisionState::NoCamera),
            DesiredCapture::Capture(_) => None,
        }
    }
}

/// Consent first: with cameras off nothing is opened, not even for discovery.
pub fn desired_capture(
    settings: &Settings,
    discover: impl FnOnce() -> Option<String>,
) -> DesiredCapture {
    if !settings.cameras_enabled {
        return DesiredCapture::ConsentOff;
    }
    if !settings.vision_enabled {
        return DesiredCapture::VisionOff;
    }
    let configured = settings.vision_camera_url.trim();
    let (source, source_kind) = if !configured.is_empty() {
        (configured.to_string(), SourceKind::Configured)
    } else if let Some(found) = discover() {
        (found, SourceKind::Discovered)
    } else {
        return DesiredCapture::NoCamera;
    };
    DesiredCapture::Capture(CaptureSpec {
        source,
        source_kind,
        camera_id: policy::camera_id(settings),
        fps: policy::clamp_fps(settings.vision_fps),
        motion_threshold: policy::clamp_motion_threshold(settings.vision_motion_threshold),
    })
}

/// Why a running pipeline must stop or restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureChange {
    ConsentRevoked,
    VisionDisabled,
    CameraGone,
    SourceChanged,
    CameraIdChanged,
    FpsChanged,
    MotionThresholdChanged,
}

impl CaptureChange {
    /// A restart keeps capturing with the new values; otherwise capture stops.
    pub fn restarts(&self) -> bool {
        !matches!(
            self,
            CaptureChange::ConsentRevoked
                | CaptureChange::VisionDisabled
                | CaptureChange::CameraGone
        )
    }

    /// The old camera's frames would be stale or no longer allowed.
    fn clears_feed(&self) -> bool {
        !matches!(
            self,
            CaptureChange::SourceChanged
                | CaptureChange::FpsChanged
                | CaptureChange::MotionThresholdChanged
        )
    }

    pub fn reason(&self) -> &'static str {
        match self {
            CaptureChange::ConsentRevoked => "camera consent turned off",
            CaptureChange::VisionDisabled => "vision turned off",
            CaptureChange::CameraGone => "no camera configured or discovered",
            CaptureChange::SourceChanged => "camera address changed",
            CaptureChange::CameraIdChanged => "camera name changed",
            CaptureChange::FpsChanged => "frame rate changed",
            CaptureChange::MotionThresholdChanged => "motion sensitivity changed",
        }
    }
}

/// `None` leaves the running pipeline alone.
pub fn capture_change(running: &CaptureSpec, desired: &DesiredCapture) -> Option<CaptureChange> {
    let next = match desired {
        DesiredCapture::ConsentOff => return Some(CaptureChange::ConsentRevoked),
        DesiredCapture::VisionOff => return Some(CaptureChange::VisionDisabled),
        DesiredCapture::NoCamera => return Some(CaptureChange::CameraGone),
        DesiredCapture::Capture(next) => next,
    };
    if next.source != running.source {
        Some(CaptureChange::SourceChanged)
    } else if next.camera_id != running.camera_id {
        Some(CaptureChange::CameraIdChanged)
    } else if next.fps != running.fps {
        Some(CaptureChange::FpsChanged)
    } else if next.motion_threshold != running.motion_threshold {
        Some(CaptureChange::MotionThresholdChanged)
    } else {
        None
    }
}

/// `rtsp://user:pass@host/x` becomes `rtsp://***@host/x`; anything without credentials is kept.
pub fn redact_source(source: &str) -> String {
    let Some((scheme, rest)) = source.split_once("://") else {
        return source.to_string();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rfind('@') {
        Some(at) => format!("{scheme}://***@{}{tail}", &authority[at + 1..]),
        None => source.to_string(),
    }
}

pub struct VisionSupervisor {
    settings: Arc<dyn SettingsRepository>,
    storage: Arc<dyn CameraStorage>,
    bus: Arc<dyn EventBus>,
    registry: LiveCameraRegistry,
    classifier: Option<Arc<dyn VisionClassifier>>,
    face_recognizer: Option<Arc<dyn FaceRecognition>>,
    snapshots: Option<SnapshotConfig>,
    open_source: SourceOpener,
    discover: CameraDiscovery,
    timing: SupervisorTiming,
}

enum RunOutcome {
    Ended(anyhow::Result<()>),
    Changed(CaptureChange),
}

impl VisionSupervisor {
    /// ffmpeg capture, V4L2 discovery and the default timing.
    pub fn new(
        settings: Arc<dyn SettingsRepository>,
        storage: Arc<dyn CameraStorage>,
        bus: Arc<dyn EventBus>,
        registry: LiveCameraRegistry,
    ) -> Self {
        Self {
            settings,
            storage,
            bus,
            registry,
            classifier: None,
            face_recognizer: None,
            snapshots: None,
            open_source: Arc::new(|cfg| {
                FfmpegFrameSource::spawn(cfg).map(|s| Box::new(s) as Box<dyn FrameSource>)
            }),
            discover: Arc::new(|| {
                crate::discovery::default_camera_source().map(|p| p.to_string_lossy().into_owned())
            }),
            timing: SupervisorTiming::default(),
        }
    }

    pub fn with_classifier(mut self, classifier: Option<Arc<dyn VisionClassifier>>) -> Self {
        self.classifier = classifier;
        self
    }

    pub fn with_face_recognizer(mut self, face: Option<Arc<dyn FaceRecognition>>) -> Self {
        self.face_recognizer = face;
        self
    }

    pub fn with_snapshots(mut self, snapshots: SnapshotConfig) -> Self {
        self.snapshots = Some(snapshots);
        self
    }

    pub fn with_source_opener(mut self, open: SourceOpener) -> Self {
        self.open_source = open;
        self
    }

    pub fn with_discovery(mut self, discover: CameraDiscovery) -> Self {
        self.discover = discover;
        self
    }

    pub fn with_timing(mut self, timing: SupervisorTiming) -> Self {
        self.timing = timing;
        self
    }

    /// Runs for the life of the process.
    pub async fn run(self) {
        let classifier_available = self.classifier.is_some();
        self.registry
            .update_status(|s| s.classifier_available = classifier_available);
        let mut backoff = self.timing.backoff_min;

        loop {
            let Some(settings) = self.read_settings().await else {
                self.wait_for_settings().await;
                continue;
            };
            let desired = desired_capture(&settings, || (self.discover)());
            let spec = match desired {
                DesiredCapture::Capture(spec) => spec,
                idle => {
                    self.mark_idle(&idle, &settings);
                    backoff = self.timing.backoff_min;
                    self.wait_for_settings().await;
                    continue;
                }
            };

            self.mark_starting(&spec);
            let shown = redact_source(&spec.source);
            let source = match (self.open_source)(&spec.capture_config()) {
                Ok(source) => source,
                Err(e) => {
                    let error = format!("{e:#}");
                    tracing::warn!(camera = %spec.camera_id, source = %shown, %error, retry_in = ?backoff, "camera unavailable; retrying");
                    self.mark_reconnecting(error);
                    match self.wait_out(backoff, &spec).await {
                        Some(change) => self.log_change(&spec, change),
                        None => backoff = (backoff * 2).min(self.timing.backoff_max),
                    }
                    continue;
                }
            };

            tracing::info!(camera = %spec.camera_id, source = %shown, fps = spec.fps, "vision supervisor started the camera pipeline");
            let started = Utc::now();
            let outcome = tokio::select! {
                ended = run_vision_pipeline(
                    source,
                    self.classifier.clone(),
                    self.face_recognizer.clone(),
                    self.storage.clone(),
                    self.bus.clone(),
                    &self.registry,
                    spec.pipeline_config(self.snapshots.clone()),
                ) => RunOutcome::Ended(ended),
                change = self.watch_for_change(&spec) => RunOutcome::Changed(change),
            };

            match outcome {
                RunOutcome::Changed(change) => {
                    self.log_change(&spec, change);
                    if change.restarts() {
                        self.registry.update_status(|s| s.restarts += 1);
                    }
                    backoff = self.timing.backoff_min;
                }
                RunOutcome::Ended(ended) => {
                    let produced = self
                        .registry
                        .last_frame_at(&spec.camera_id)
                        .is_some_and(|at| at >= started);
                    if produced {
                        backoff = self.timing.backoff_min;
                    }
                    let error = match ended {
                        Ok(()) => "camera stream ended".to_string(),
                        Err(e) => format!("{e:#}"),
                    };
                    tracing::warn!(camera = %spec.camera_id, source = %shown, %error, retry_in = ?backoff, "camera stream stopped; vision supervisor will reconnect");
                    self.mark_reconnecting(error);
                    self.registry.update_status(|s| s.restarts += 1);
                    match self.wait_out(backoff, &spec).await {
                        Some(change) => self.log_change(&spec, change),
                        None if !produced => backoff = (backoff * 2).min(self.timing.backoff_max),
                        None => {}
                    }
                }
            }
        }
    }

    async fn read_settings(&self) -> Option<Settings> {
        match self.settings.get().await {
            Ok(settings) => {
                self.registry.sync_settings(&settings);
                Some(settings)
            }
            Err(e) => {
                tracing::warn!(error = %e, "vision supervisor could not read settings");
                None
            }
        }
    }

    async fn wait_for_settings(&self) {
        tokio::select! {
            _ = tokio::time::sleep(self.timing.poll) => {}
            _ = self.registry.settings_changed() => {}
        }
    }

    /// Returns the first settings change that affects `running`; never returns otherwise.
    async fn watch_for_change(&self, running: &CaptureSpec) -> CaptureChange {
        loop {
            self.wait_for_settings().await;
            let Some(settings) = self.read_settings().await else {
                continue;
            };
            let desired = desired_capture(&settings, || (self.discover)());
            if let Some(change) = capture_change(running, &desired) {
                return change;
            }
        }
    }

    /// Sleeps `backoff`, cut short by a settings change that affects `spec`.
    async fn wait_out(&self, backoff: Duration, spec: &CaptureSpec) -> Option<CaptureChange> {
        tokio::select! {
            _ = tokio::time::sleep(backoff) => None,
            change = self.watch_for_change(spec) => Some(change),
        }
    }

    fn log_change(&self, spec: &CaptureSpec, change: CaptureChange) {
        if change.clears_feed() {
            self.registry.clear_camera(&spec.camera_id);
        }
        if change.restarts() {
            tracing::info!(camera = %spec.camera_id, reason = change.reason(), "vision supervisor restarting the camera pipeline");
        } else {
            tracing::info!(camera = %spec.camera_id, reason = change.reason(), "vision supervisor stopped the camera pipeline");
        }
    }

    fn mark_idle(&self, desired: &DesiredCapture, settings: &Settings) {
        let Some(state) = desired.idle_state() else {
            return;
        };
        let camera_id = policy::camera_id(settings);
        self.registry.update_status(|s| {
            s.state = state;
            s.camera_id = Some(camera_id);
            s.source = None;
            s.source_kind = None;
            s.fps = None;
            s.last_error = None;
        });
    }

    fn mark_starting(&self, spec: &CaptureSpec) {
        let source = redact_source(&spec.source);
        self.registry.update_status(|s| {
            if s.state != VisionState::Reconnecting {
                s.state = VisionState::Starting;
            }
            s.camera_id = Some(spec.camera_id.clone());
            s.source = Some(source);
            s.source_kind = Some(spec.source_kind);
            s.fps = Some(spec.fps);
        });
    }

    fn mark_reconnecting(&self, error: String) {
        self.registry.update_status(|s| {
            s.state = VisionState::Reconnecting;
            s.last_error = Some(error);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
    use pond_core::user_data::domain::vision::Frame;
    use pond_core::user_data::mocks::mock_sensor::MockCameraStorage;
    use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn enabled() -> Settings {
        Settings {
            vision_enabled: true,
            cameras_enabled: true,
            vision_camera_url: "rtsp://cam.local/stream".into(),
            vision_camera_id: "porch".into(),
            vision_fps: 2,
            vision_motion_threshold: 0.05,
            ..Settings::default()
        }
    }

    fn spec_of(settings: &Settings) -> CaptureSpec {
        match desired_capture(settings, || None) {
            DesiredCapture::Capture(spec) => spec,
            other => panic!("expected a capture, got {other:?}"),
        }
    }

    #[test]
    fn consent_off_wins_over_everything_and_never_runs_discovery() {
        let settings = Settings {
            cameras_enabled: false,
            ..enabled()
        };
        let desired = desired_capture(&settings, || panic!("discovery must not run"));
        assert_eq!(desired, DesiredCapture::ConsentOff);

        let settings = Settings {
            vision_enabled: false,
            ..enabled()
        };
        assert_eq!(
            desired_capture(&settings, || None),
            DesiredCapture::VisionOff
        );
    }

    #[test]
    fn a_configured_url_beats_discovery_and_an_empty_one_falls_back_to_it() {
        let configured = spec_of(&enabled());
        assert_eq!(configured.source, "rtsp://cam.local/stream");
        assert_eq!(configured.source_kind, SourceKind::Configured);

        let blank = Settings {
            vision_camera_url: "  ".into(),
            ..enabled()
        };
        assert_eq!(desired_capture(&blank, || None), DesiredCapture::NoCamera);
        match desired_capture(&blank, || Some("/dev/video0".into())) {
            DesiredCapture::Capture(spec) => {
                assert_eq!(spec.source, "/dev/video0");
                assert_eq!(spec.source_kind, SourceKind::Discovered);
            }
            other => panic!("expected the discovered camera, got {other:?}"),
        }
    }

    #[test]
    fn the_spec_carries_clamped_values() {
        let spec = spec_of(&Settings {
            vision_fps: 30,
            vision_motion_threshold: 0.9,
            vision_camera_id: " ".into(),
            ..enabled()
        });
        assert_eq!(spec.fps, 5);
        assert_eq!(spec.motion_threshold, 0.5);
        assert_eq!(spec.camera_id, policy::DEFAULT_CAMERA_ID);
    }

    #[test]
    fn each_capture_setting_change_restarts_and_nothing_else_does() {
        let base = enabled();
        let running = spec_of(&base);

        let unchanged = Settings {
            vision_event_cooldown_secs: 99,
            vision_event_types: vec!["person".into()],
            vision_face_recognition_enabled: false,
            ..base.clone()
        };
        assert_eq!(
            capture_change(&running, &desired_capture(&unchanged, || None)),
            None,
            "bus settings apply live without a restart"
        );

        let cases: [(Settings, CaptureChange); 4] = [
            (
                Settings {
                    vision_camera_url: "rtsp://other/stream".into(),
                    ..base.clone()
                },
                CaptureChange::SourceChanged,
            ),
            (
                Settings {
                    vision_camera_id: "garage".into(),
                    ..base.clone()
                },
                CaptureChange::CameraIdChanged,
            ),
            (
                Settings {
                    vision_fps: 4,
                    ..base.clone()
                },
                CaptureChange::FpsChanged,
            ),
            (
                Settings {
                    vision_motion_threshold: 0.2,
                    ..base.clone()
                },
                CaptureChange::MotionThresholdChanged,
            ),
        ];
        for (settings, want) in cases {
            let got = capture_change(&running, &desired_capture(&settings, || None));
            assert_eq!(got, Some(want));
            assert!(want.restarts(), "{want:?} keeps capturing");
        }

        // A value that clamps to the running one is not a change.
        let clamped_same = Settings {
            vision_fps: 2,
            vision_motion_threshold: 0.05,
            ..base.clone()
        };
        assert_eq!(
            capture_change(&running, &desired_capture(&clamped_same, || None)),
            None
        );
    }

    #[test]
    fn turning_vision_or_consent_off_stops_rather_than_restarts() {
        let running = spec_of(&enabled());
        for (desired, want) in [
            (DesiredCapture::ConsentOff, CaptureChange::ConsentRevoked),
            (DesiredCapture::VisionOff, CaptureChange::VisionDisabled),
            (DesiredCapture::NoCamera, CaptureChange::CameraGone),
        ] {
            let got = capture_change(&running, &desired).expect("must stop");
            assert_eq!(got, want);
            assert!(!got.restarts());
        }
    }

    #[test]
    fn credentials_never_reach_the_status() {
        assert_eq!(
            redact_source("rtsp://admin:hunter2@192.168.1.20:554/live?ch=1"),
            "rtsp://***@192.168.1.20:554/live?ch=1"
        );
        assert_eq!(
            redact_source("http://user@cam.local/mjpeg"),
            "http://***@cam.local/mjpeg"
        );
        assert_eq!(
            redact_source("rtsp://cam.local/a@b"),
            "rtsp://cam.local/a@b",
            "an @ in the path is not a credential"
        );
        assert_eq!(redact_source("/dev/video0"), "/dev/video0");
    }

    /// A source that yields a frame every few milliseconds until dropped.
    struct EndlessSource;

    #[async_trait]
    impl FrameSource for EndlessSource {
        async fn next_frame(&mut self) -> anyhow::Result<Option<Frame>> {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok(Some(Frame {
                width: 8,
                height: 8,
                rgb: vec![10; Frame::expected_len(8, 8)],
                captured_at: Utc::now(),
            }))
        }
    }

    fn fast() -> SupervisorTiming {
        SupervisorTiming {
            poll: Duration::from_millis(20),
            backoff_min: Duration::from_millis(20),
            backoff_max: Duration::from_millis(40),
        }
    }

    async fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    #[tokio::test]
    async fn the_supervisor_follows_settings_live() {
        let settings = Arc::new(MockSettingsRepository::new());
        settings.update(&enabled()).await.unwrap();
        let registry = LiveCameraRegistry::new();
        let opened = Arc::new(AtomicUsize::new(0));
        let opens = opened.clone();
        let supervisor = VisionSupervisor::new(
            settings.clone(),
            Arc::new(MockCameraStorage::new()),
            Arc::new(InProcessEventBus::new()),
            registry.clone(),
        )
        .with_timing(fast())
        .with_discovery(Arc::new(|| None))
        .with_source_opener(Arc::new(move |_| {
            opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(EndlessSource) as Box<dyn FrameSource>)
        }));
        let task = tokio::spawn(supervisor.run());

        wait_until("running", || {
            registry.status().state == VisionState::Running
        })
        .await;
        assert!(registry.is_live("porch"));
        assert_eq!(opened.load(Ordering::SeqCst), 1);

        // A capture setting change restarts the pipeline with the new value.
        settings
            .update(&Settings {
                vision_fps: 4,
                ..enabled()
            })
            .await
            .unwrap();
        wait_until("restart", || opened.load(Ordering::SeqCst) == 2).await;
        wait_until("running at 4 fps", || {
            let s = registry.status();
            s.state == VisionState::Running && s.fps == Some(4)
        })
        .await;
        assert_eq!(registry.status().restarts, 1);

        // Revoking consent stops capture and drops the cached frame.
        settings
            .update(&Settings {
                cameras_enabled: false,
                vision_fps: 4,
                ..enabled()
            })
            .await
            .unwrap();
        wait_until("consent off", || {
            registry.status().state == VisionState::ConsentOff
        })
        .await;
        assert!(registry.get_latest_frame("porch").is_none());
        let opens_when_revoked = opened.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            opened.load(Ordering::SeqCst),
            opens_when_revoked,
            "no capture is attempted while consent is off"
        );
        assert!(registry.get_latest_frame("porch").is_none());

        // Vision off reads as off once consent is back.
        settings
            .update(&Settings {
                vision_enabled: false,
                ..enabled()
            })
            .await
            .unwrap();
        wait_until("off", || registry.status().state == VisionState::Off).await;
        task.abort();
    }

    #[tokio::test]
    async fn a_dying_stream_is_reconnected_and_its_error_shown_until_frames_return() {
        let settings = Arc::new(MockSettingsRepository::new());
        settings.update(&enabled()).await.unwrap();
        let registry = LiveCameraRegistry::new();
        let attempts = Arc::new(AtomicUsize::new(0));
        let tries = attempts.clone();
        let supervisor = VisionSupervisor::new(
            settings,
            Arc::new(MockCameraStorage::new()),
            Arc::new(InProcessEventBus::new()),
            registry.clone(),
        )
        .with_timing(fast())
        .with_source_opener(Arc::new(move |_| {
            if tries.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("connection refused")
            }
            Ok(Box::new(EndlessSource) as Box<dyn FrameSource>)
        }));
        let task = tokio::spawn(supervisor.run());

        wait_until("the failure is visible", || {
            registry
                .status()
                .last_error
                .is_some_and(|e| e.contains("connection refused"))
        })
        .await;
        wait_until("recovered", || {
            registry.status().state == VisionState::Running
        })
        .await;
        assert_eq!(registry.status().last_error, None);
        task.abort();
    }

    #[tokio::test]
    async fn a_settings_save_wakes_the_supervisor_before_its_poll() {
        let settings = Arc::new(MockSettingsRepository::new());
        settings.update(&enabled()).await.unwrap();
        let registry = LiveCameraRegistry::new();
        let supervisor = VisionSupervisor::new(
            settings.clone(),
            Arc::new(MockCameraStorage::new()),
            Arc::new(InProcessEventBus::new()),
            registry.clone(),
        )
        .with_timing(SupervisorTiming {
            poll: Duration::from_secs(3600),
            ..fast()
        })
        .with_source_opener(Arc::new(|_| {
            Ok(Box::new(EndlessSource) as Box<dyn FrameSource>)
        }));
        let task = tokio::spawn(supervisor.run());
        wait_until("running", || {
            registry.status().state == VisionState::Running
        })
        .await;

        settings
            .update(&Settings {
                vision_enabled: false,
                ..enabled()
            })
            .await
            .unwrap();
        registry.notify_settings_changed();
        wait_until("stopped on notify", || {
            registry.status().state == VisionState::Off
        })
        .await;
        task.abort();
    }
}
