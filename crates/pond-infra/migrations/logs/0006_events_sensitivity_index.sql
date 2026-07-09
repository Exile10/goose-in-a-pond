-- Index on `privacy_sensitivity` for the unified `events` log (#117).
--
-- Sensitivity-aware retention (Q2-40) sweeps events by privacy class
-- (`DELETE ... WHERE privacy_sensitivity IN (...)`); this index keeps that
-- pruning pass cheap as the log grows. Complements the existing timestamp /
-- category / session indexes from migration 0004.
CREATE INDEX IF NOT EXISTS idx_events_sensitivity ON events (privacy_sensitivity);
