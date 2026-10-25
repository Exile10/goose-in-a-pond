use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use tokio::sync::broadcast;

const BROADCAST_CAPACITY: usize = 16;

#[derive(Clone)]
pub struct LiveCameraRegistry {
    feeds: Arc<RwLock<HashMap<String, LiveFeedEntry>>>,
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
        Self {
            feeds: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<LiveCameraRegistry> = OnceLock::new();
        REGISTRY.get_or_init(LiveCameraRegistry::new)
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
