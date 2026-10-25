use pond_core::user_data::domain::sensor::CameraEvent;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use tokio::sync::broadcast;

const BROADCAST_CAPACITY: usize = 16;
const EVENT_BROADCAST_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub struct VisionBusConfig {
    pub min_event_interval_secs: u64,
    pub classifier_confidence_threshold: f64,
    pub face_detection_enabled: bool,
    pub face_on_motion: bool,
    pub enabled_event_types: Vec<String>,
}

impl Default for VisionBusConfig {
    fn default() -> Self {
        Self {
            min_event_interval_secs: 10,
            classifier_confidence_threshold: 0.5,
            face_detection_enabled: true,
            face_on_motion: true,
            enabled_event_types: vec![
                "motion".to_string(),
                "person".to_string(),
                "pet".to_string(),
                "vehicle".to_string(),
                "package".to_string(),
                "face_detected".to_string(),
                "face_recognized".to_string(),
                "face_unknown".to_string(),
            ],
        }
    }
}

impl VisionBusConfig {
    pub fn supported_signals() -> &'static [&'static str] {
        &[
            "motion",
            "person",
            "pet",
            "vehicle",
            "package",
            "face_detected",
            "face_recognized",
            "face_unknown",
            "face:<profile_id>",
        ]
    }

    pub fn is_event_enabled(&self, event_type: &str) -> bool {
        self.enabled_event_types.is_empty()
            || self
                .enabled_event_types
                .iter()
                .any(|e| e.eq_ignore_ascii_case(event_type))
    }
}

#[derive(Clone)]
pub struct LiveCameraRegistry {
    feeds: Arc<RwLock<HashMap<String, LiveFeedEntry>>>,
    config: Arc<RwLock<VisionBusConfig>>,
    events_tx: broadcast::Sender<CameraEvent>,
}

#[derive(Clone)]
struct LiveFeedEntry {
    latest_jpeg: Arc<RwLock<Option<Vec<u8>>>>,
    tx: broadcast::Sender<Arc<Vec<u8>>>,
}

impl LiveFeedEntry {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            latest_jpeg: Arc::new(RwLock::new(None)),
            tx,
        }
    }
}

impl LiveCameraRegistry {
    pub fn new() -> Self {
        let (events_tx, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        Self {
            feeds: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(VisionBusConfig::default())),
            events_tx,
        }
    }

    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<LiveCameraRegistry> = OnceLock::new();
        REGISTRY.get_or_init(LiveCameraRegistry::new)
    }

    pub fn bus_config(&self) -> VisionBusConfig {
        self.config.read().unwrap().clone()
    }

    pub fn set_bus_config(&self, cfg: VisionBusConfig) {
        *self.config.write().unwrap() = cfg;
    }

    pub fn update_bus_config<F: FnOnce(&mut VisionBusConfig)>(&self, f: F) -> VisionBusConfig {
        let mut guard = self.config.write().unwrap();
        f(&mut guard);
        guard.clone()
    }

    pub fn publish_event(&self, event: CameraEvent) {
        let _ = self.events_tx.send(event);
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<CameraEvent> {
        self.events_tx.subscribe()
    }

    pub fn event_listener_count(&self) -> usize {
        self.events_tx.receiver_count()
    }

    pub fn update_frame(&self, camera_id: &str, jpeg: Vec<u8>) {
        let entry = {
            let mut feeds = self.feeds.write().unwrap();
            feeds
                .entry(camera_id.to_string())
                .or_insert_with(LiveFeedEntry::new)
                .clone()
        };
        *entry.latest_jpeg.write().unwrap() = Some(jpeg.clone());
        let _ = entry.tx.send(Arc::new(jpeg));
    }

    pub fn get_latest_frame(&self, camera_id: &str) -> Option<Vec<u8>> {
        let feeds = self.feeds.read().unwrap();
        feeds
            .get(camera_id)
            .and_then(|entry| entry.latest_jpeg.read().unwrap().clone())
    }

    pub fn subscribe(&self, camera_id: &str) -> broadcast::Receiver<Arc<Vec<u8>>> {
        let mut feeds = self.feeds.write().unwrap();
        let entry = feeds
            .entry(camera_id.to_string())
            .or_insert_with(LiveFeedEntry::new);
        entry.tx.subscribe()
    }

    pub fn active_cameras(&self) -> Vec<String> {
        let feeds = self.feeds.read().unwrap();
        feeds.keys().cloned().collect()
    }
}

impl Default for LiveCameraRegistry {
    fn default() -> Self {
        Self::new()
    }
}
