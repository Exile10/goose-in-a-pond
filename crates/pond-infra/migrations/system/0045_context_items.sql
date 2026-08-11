-- PAI-8 P1 -- the personal-context corpus.
--
-- One row per thing a source produced. This is deliberately NOT the
-- `memory_fragments` table: memory is a curated store with decay, consolidation
-- and importance, and a mailbox poured into it would drown it (section 3.2).
-- It is a second corpus with its own retrieval budget.
--
-- ── Against a database that already has rows ───────────────────────────────
--
-- A new table. Every existing pond gets an empty one; no read that runs today
-- touches it and no backfill is possible or wanted.
--
-- ── What is denormalised, and why that is safe ─────────────────────────────
--
-- `profile_id` and `source_kind` are both derivable by joining `context_sources`.
-- They are copied here because every scoped read filters on the owner and every
-- retention sweep filters on the kind, and a two-table join for the hot path of
-- a per-turn retrieval on a Jetson is not free. 0044 makes the copy safe by
-- refusing to let either change on the source, and the trigger below makes it
-- consistent by refusing an item whose owner is not its source's.

CREATE TABLE IF NOT EXISTS context_items (
    id           TEXT PRIMARY KEY,
    source_id    TEXT NOT NULL REFERENCES context_sources(id) ON DELETE CASCADE,

    -- The upstream's own identifier. What makes a re-sync idempotent: a cursor
    -- slipping backwards is normal, and a store that answered it with duplicates
    -- would fill the corpus with the same three messages.
    external_id  TEXT NOT NULL,

    -- PAI-8 invariant 1. See 0044's note; the same CHECK for the same reason.
    profile_id   TEXT NOT NULL,

    -- The SOURCE's kind, denormalised. Retention buckets on it.
    source_kind  TEXT NOT NULL,
    -- message | event | document | location | task.
    item_kind    TEXT NOT NULL,

    -- When the thing HAPPENED. May be in the future: a calendar event is the
    -- point of ingesting a calendar, which is why the retrieval blend's
    -- proximity term is symmetric about now rather than a one-sided decay.
    occurred_at  TEXT NOT NULL,
    -- When this pond learned of it.
    ingested_at  TEXT NOT NULL,

    -- Already redacted when written, and re-redacted when read.
    -- `ContextItem::from_parts` is the only constructor and it takes the
    -- `Redactor`, so there is no path -- ingest or storage -- that produces one
    -- of these values unredacted. PAI-8 invariant 3.
    title        TEXT NOT NULL,
    body         TEXT NOT NULL,
    -- JSON array, redacted with the same pass as the body.
    participants TEXT NOT NULL DEFAULT '[]',

    -- public | internal | sensitive | secret, though `secret` is unreachable by
    -- construction: it means credentials, and credentials are what the ingest
    -- redaction level removes. Derived, never supplied by a caller, and read
    -- back through the same derivation -- a value lowered out of band loses to
    -- the computed one, because sensitivity is a restriction.
    sensitivity  TEXT NOT NULL,

    -- Raw f32 vector, as `memory_fragments.embedding` stores it. NULL until the
    -- backfill embeds it; computed over the REDACTED text, which is the only
    -- text this row has ever held.
    embedding    BLOB,

    UNIQUE (source_id, external_id),
    CHECK (trim(profile_id) <> ''),
    CHECK (trim(external_id) <> '')
);

-- The retrieval read path: this owner's items, newest first.
CREATE INDEX IF NOT EXISTS idx_context_items_profile_occurred
    ON context_items(profile_id, occurred_at DESC);

-- The retention sweep: one bucket per (source kind, sensitivity class).
CREATE INDEX IF NOT EXISTS idx_context_items_retention
    ON context_items(source_kind, sensitivity, occurred_at);

-- ── An item belongs to its source's owner ─────────────────────────────────
--
-- The denormalised `profile_id` is only trustworthy if it cannot disagree with
-- the source's. Rust takes the owner from the source and never from the payload
-- (`IngestPipeline::ingest`), and a payload that named its own owner would be
-- the same hole PAI-1 P4 closed on `PUT /sessions/{id}/user`. This is the layer
-- under that, and it also catches the case Rust cannot see: a row inserted by
-- anything other than the pipeline.
--
-- A missing source makes the SELECT return NULL, `IS NOT` is then true, and the
-- insert aborts. That is the right direction: an item whose source cannot be
-- found has no owner anybody can vouch for. (The foreign key would refuse it
-- anyway when `PRAGMA foreign_keys` is on, which `Database::connect` sets on
-- every pooled connection -- but this trigger does not depend on that pragma
-- being on, and the pragma is per-connection.)
CREATE TRIGGER IF NOT EXISTS trg_context_items_owner_matches_source_on_insert
BEFORE INSERT ON context_items
FOR EACH ROW
WHEN NEW.profile_id IS NOT (SELECT profile_id FROM context_sources WHERE id = NEW.source_id)
BEGIN
    SELECT RAISE(ABORT, 'a context item must belong to the same household member as its source');
END;

-- On UPDATE as well, and on the whole row rather than `UPDATE OF profile_id`:
-- `BEFORE UPDATE OF profile_id` only fires when that column is in the SET list,
-- so `UPDATE context_items SET source_id = ...` would walk straight past it.
-- 0041 recorded the same trap for the proposal rationale triggers.
CREATE TRIGGER IF NOT EXISTS trg_context_items_owner_matches_source_on_update
BEFORE UPDATE ON context_items
FOR EACH ROW
WHEN NEW.profile_id IS NOT (SELECT profile_id FROM context_sources WHERE id = NEW.source_id)
BEGIN
    SELECT RAISE(ABORT, 'a context item must belong to the same household member as its source');
END;
