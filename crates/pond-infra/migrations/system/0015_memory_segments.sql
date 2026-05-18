-- Memory segments, importance scoring, decay, and lifecycle management.
-- All columns are optional/defaulted for backward compatibility with existing rows.

ALTER TABLE memory_fragments ADD COLUMN segment TEXT;
ALTER TABLE memory_fragments ADD COLUMN importance REAL;
ALTER TABLE memory_fragments ADD COLUMN tier TEXT DEFAULT 'long';
ALTER TABLE memory_fragments ADD COLUMN decay_rate REAL;
ALTER TABLE memory_fragments ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memory_fragments ADD COLUMN last_accessed_at TEXT;
ALTER TABLE memory_fragments ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';
ALTER TABLE memory_fragments ADD COLUMN superseded_by TEXT;

CREATE INDEX IF NOT EXISTS idx_memory_fragments_segment ON memory_fragments(segment);
CREATE INDEX IF NOT EXISTS idx_memory_fragments_lifecycle ON memory_fragments(lifecycle);
