-- camera_events: vision detection events from camera streams.
-- TTL: 14 days for acknowledged events (unacknowledged alerts kept indefinitely).

CREATE TABLE IF NOT EXISTS camera_events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id     TEXT    NOT NULL,
    event_type    TEXT    NOT NULL,
    confidence    REAL,
    snapshot_path TEXT,
    metadata      TEXT,
    acknowledged  INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_camera_events_created_at
    ON camera_events(created_at);

CREATE INDEX IF NOT EXISTS idx_camera_events_camera_id
    ON camera_events(camera_id);
