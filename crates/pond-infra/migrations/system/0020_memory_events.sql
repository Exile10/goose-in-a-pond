-- Memory audit log: tracks every lifecycle event for debugging.
CREATE TABLE IF NOT EXISTS memory_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    event_kind TEXT    NOT NULL,
    memory_id  TEXT    NOT NULL,
    session_id TEXT,
    data       TEXT,
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_memory_events_memory_id ON memory_events(memory_id);
CREATE INDEX idx_memory_events_kind ON memory_events(event_kind);
CREATE INDEX idx_memory_events_created ON memory_events(created_at);
