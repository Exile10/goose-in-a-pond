-- PAI-8 P1 -- where personal context comes from.
--
-- A context SOURCE is an account, sensor or device that produces data about one
-- household member. This migration and 0045 are the storage half of
-- docs/architecture/pai/08-personal-context-streaming.md section 3.1. (That
-- document proposes the numbers 0041 and 0042; both were taken by PAI-7 before
-- this landed, which is why it also says to list the directory rather than
-- trust the line.)
--
-- ── Against a database that already has rows ───────────────────────────────
--
-- A new table, so every existing pond gets an empty one and nothing that runs
-- today changes behaviour. The triggers below fire only on rows this feature
-- writes, of which there are none anywhere yet. There is no backfill and none
-- would be honest: no pond has ever recorded where its context came from.
--
-- ── Why there is no token column ──────────────────────────────────────────
--
-- PAI-8 invariant 4: connector tokens live in the encrypted secret store
-- (`SecretRepository`, `$DATA_DIR/secrets.json`, XChaCha20-Poly1305), never in
-- `Settings` and never here. `secret_ref` holds the KEY to look one up, which is
-- not a credential. A column a token could be written to is a column a token
-- eventually is written to, so the schema does not offer one -- and
-- `the_schema_has_nowhere_to_put_a_token` in sqlite_context.rs asserts it
-- against the live table rather than against this comment.

CREATE TABLE IF NOT EXISTS context_sources (
    id          TEXT PRIMARY KEY,
    -- sensor | camera | voice | mobile | mail | calendar | files | chat.
    -- Written by SourceKind::as_str and read by SourceKind::parse; an
    -- unrecognised value is refused on read, which narrows.
    kind        TEXT NOT NULL,
    provider    TEXT NOT NULL,

    -- PAI-8 invariant 1, and PAI-1 section 1.2's lesson before it: NOT NULL,
    -- because an `Option` owner becomes `None` at every call site. The CHECK is
    -- what stops '' being the new NULL.
    --
    -- ON DELETE CASCADE, and it is the only coherent action here. SET NULL
    -- cannot apply to a NOT NULL column, and leaving the row would keep one
    -- person's mailbox in the pond after they have been removed from the
    -- household -- which is the opposite of what removing them means. 0043 chose
    -- SET NULL for `devices` deliberately and for a different reason: a device
    -- is a destination that outlives its owner, while this is content that must
    -- not.
    profile_id  TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,

    -- JSON array of the scopes the user actually granted. Not a permission
    -- check: it is what the pond was allowed to ask for, kept so the UI can say
    -- so and so a re-auth can ask for the same set.
    scopes      TEXT NOT NULL DEFAULT '[]',
    -- Incremental sync position, opaque to GIAP.
    cursor      TEXT,
    last_sync   TEXT,
    -- connected | needs_reauth | error | paused. Unrecognised reads as `error`,
    -- never as `connected`: an unreadable status must not be the one that says
    -- "carry on syncing".
    status      TEXT NOT NULL DEFAULT 'connected',
    -- The KEY into SecretRepository, never a secret. See the header.
    secret_ref  TEXT,
    created_at  TEXT NOT NULL,

    CHECK (trim(id) <> ''),
    CHECK (trim(profile_id) <> ''),
    CHECK (trim(kind) <> '')
);

CREATE INDEX IF NOT EXISTS idx_context_sources_profile ON context_sources(profile_id);

-- ── A source's owner and kind are its identity ────────────────────────────
--
-- Both are denormalised onto every item in 0045: the owner because that is what
-- every scoped read filters on, and the kind because it is what retention and
-- the rendered prompt line need. Denormalisation is safe exactly as long as
-- neither can change under the items already stored, so neither can change.
--
-- Rust refuses this too -- `ContextSource::advance` can only touch the cursor,
-- the sync time and the status, and there are no other setters. This is the
-- layer under that: the Rust guard is a method that can be added to, and this
-- one cannot be got past without a migration.
--
-- `IS NOT` rather than `<>`: SQLite's `<>` is NULL when either side is NULL, and
-- a NULL WHEN clause does not fire. Both columns are NOT NULL today, so `<>`
-- would work today -- and would stop working silently the day one of them
-- became nullable, which is precisely the kind of change nobody re-reads the
-- triggers for.
CREATE TRIGGER IF NOT EXISTS trg_context_sources_owner_is_immutable
BEFORE UPDATE ON context_sources
FOR EACH ROW
WHEN NEW.profile_id IS NOT OLD.profile_id
BEGIN
    SELECT RAISE(ABORT, 'a context source cannot change owner: its items are already stored under the old one');
END;

CREATE TRIGGER IF NOT EXISTS trg_context_sources_kind_is_immutable
BEFORE UPDATE ON context_sources
FOR EACH ROW
WHEN NEW.kind IS NOT OLD.kind
BEGIN
    SELECT RAISE(ABORT, 'a context source cannot change kind: retention and sensitivity are derived from it');
END;
