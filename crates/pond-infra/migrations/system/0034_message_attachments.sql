-- Phase F2: persist chat image attachments so a follow-up question about an
-- earlier picture can still be answered after a trim, a compaction rebuild, or
-- a server restart.
--
-- The BYTES are NOT stored here. pond_system.db is the authoritative hot store,
-- read on every turn and on every session listing; a megabyte-per-row BLOB
-- would bloat its page cache for data that is only ever fetched whole and
-- rarely. The bytes live under <data_dir>/attachments/<session_id>/ and this
-- table is the index. `file_path` is absolute and is for operators debugging a
-- session by hand -- nothing above the storage adapter parses it.
--
-- Deleting a session cascades these rows away; the files are swept separately
-- by the storage adapter, because row-level retention pruning
-- (prune_session_messages) deletes rows without going through any adapter.

CREATE TABLE IF NOT EXISTS message_attachments (
    id          TEXT PRIMARY KEY,
    message_id  TEXT NOT NULL,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    -- Position within the message, 0-based. Preserves "the first picture".
    ordinal     INTEGER NOT NULL,
    mime_type   TEXT NOT NULL,
    byte_size   INTEGER NOT NULL,
    file_path   TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The two access patterns: "every attachment in this conversation, in order"
-- (history replay planning, UI hydration) and "the images on these messages"
-- (batched replay load).
CREATE INDEX IF NOT EXISTS idx_message_attachments_session
    ON message_attachments(session_id, created_at, ordinal);
CREATE INDEX IF NOT EXISTS idx_message_attachments_message
    ON message_attachments(message_id, ordinal);
