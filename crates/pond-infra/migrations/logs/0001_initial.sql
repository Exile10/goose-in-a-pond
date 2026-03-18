-- Logs database: telemetry and audit trail
-- Migration 0001: Initial schema

CREATE TABLE IF NOT EXISTS event_log (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT    NOT NULL DEFAULT (datetime('now')),
    level     TEXT    NOT NULL DEFAULT 'INFO',
    source    TEXT    NOT NULL,
    message   TEXT    NOT NULL,
    metadata  TEXT
);

CREATE TABLE IF NOT EXISTS system_info (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT    NOT NULL DEFAULT (datetime('now')),
    key       TEXT    NOT NULL,
    value     TEXT    NOT NULL
);
