-- The personal-context index (phase A).
--
-- This lives in its own database file, `pond_vectors.db`, and everything about
-- the shape below follows from that being DERIVED data:
--
--   * It is rebuildable. Delete the file and the pond re-embeds; nothing a
--     member ever told this household is stored only here.
--   * It never syncs to a phone. The authoritative rows do.
--   * It keeps any future native vector extension out of the authoritative
--     database's blast radius.
--
-- # There is deliberately NO text column
--
-- SQLite cross-database transactions are not atomic under WAL, and both pools
-- assert WAL. So a source row and its vector cannot be deleted together, and an
-- orphan is inevitable rather than exceptional. With no text, an orphan is
-- harmless noise: it resolves to nothing on the join and drops out of results.
-- With a cached snippet it would be deleted data that survived a deletion
-- promise -- the one failure this design will not accept.
--
-- For the same reason there are no foreign keys here. They cannot span database
-- files in SQLite, so `prune_orphans` is the reconciliation, not a constraint.
CREATE TABLE IF NOT EXISTS vectors (
    -- Which corpus the row belongs to: 'memory' | 'context' | 'summary'. Lets
    -- the three share retrieval without sharing guarantees, and lets a result
    -- say where it came from.
    corpus       TEXT NOT NULL,

    -- The source row's primary key, in ITS database. Not a foreign key -- see
    -- above. (corpus, row_id) is the identity, which makes a write an upsert:
    -- a rolling summary is overwritten in place, so appending a second vector
    -- would leave one describing a conversation that no longer exists.
    row_id       TEXT NOT NULL,

    -- Which embedder produced this. A vector from a different model still
    -- scores plausibly and is WRONG, so a mismatch has to be detectable rather
    -- than silent. Retrieval filters on it; it is not advisory.
    model_id     TEXT NOT NULL,

    -- Width, stored so a mismatch is detectable without decoding the blob and
    -- so a model swap can be counted before it is repaired.
    dims         INTEGER NOT NULL,

    -- Packed little-endian f32, the same encoding `sqlite_memory` uses.
    vector       BLOB NOT NULL,

    -- A freshness marker copied from the source row (its `updated_at`, or the
    -- summary's `rolling_summary_updated_at`). Nullable because not every
    -- corpus has one. This is what lets staleness be a LEFT JOIN rather than a
    -- durable queue: a queue fails silently -- a dropped entry is an item never
    -- searchable, with nothing left to notice it -- whereas a join recomputes
    -- the truth every time and is self-healing after a crash.
    source_rev   TEXT,

    embedded_at  TEXT NOT NULL,

    PRIMARY KEY (corpus, row_id)
);

-- Retrieval always filters by model first: mixing spaces is refused, not scored.
CREATE INDEX IF NOT EXISTS idx_vectors_model ON vectors (model_id);

-- The sweep walks one corpus at a time so a long re-embed can be interrupted
-- and resumed without re-reading the others.
CREATE INDEX IF NOT EXISTS idx_vectors_corpus_model ON vectors (corpus, model_id);
