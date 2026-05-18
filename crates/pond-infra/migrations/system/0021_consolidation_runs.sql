-- Consolidation run audit trail.
-- Stores the full result of each consolidation pass (single or adversarial).
CREATE TABLE IF NOT EXISTS consolidation_runs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT,
    mode         TEXT    NOT NULL,   -- 'single' or 'adversarial'
    memory_count INTEGER NOT NULL,
    accepted     INTEGER NOT NULL DEFAULT 0,
    rejected     INTEGER NOT NULL DEFAULT 0,
    duration_ms  INTEGER NOT NULL DEFAULT 0,
    details      TEXT               -- Full JSON of exchanges/actions
);
CREATE INDEX idx_consolidation_runs_started ON consolidation_runs(started_at);
