-- Unified observability event log (#108 / #109).
--
-- One append-only table the whole pipeline emits into, replacing the four
-- disjoint silos (event_log, TurnMetrics, sensors, camera) that had no
-- correlation keys. Backs `SqliteEventLog` (pond_core::ports::event_log::EventLog).
--
-- `attributes` holds the JSON serialization of the *typed*
-- `BTreeMap<String, AttributeValue>` from the `Event` domain type — the typing
-- and validation live at the domain/port boundary (#108); this column is
-- persistence only (not the old untyped, free-form `metadata` blob).
--
-- `privacy_sensitivity` is stored per-row so retention/export policy (Q2-40)
-- can drop or mask sensitive events without parsing their contents.

CREATE TABLE IF NOT EXISTS events (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp           TEXT NOT NULL,
    category            TEXT NOT NULL,
    action              TEXT NOT NULL,
    session_id          TEXT,
    trace_id            TEXT,
    attributes          TEXT NOT NULL DEFAULT '{}',
    privacy_sensitivity TEXT NOT NULL
);

-- Query axes called out by the issue: time window, category, and session.
CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events (timestamp);
CREATE INDEX IF NOT EXISTS idx_events_category  ON events (category);
CREATE INDEX IF NOT EXISTS idx_events_session   ON events (session_id);
