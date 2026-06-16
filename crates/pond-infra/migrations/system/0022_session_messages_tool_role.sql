-- Migration 0022: allow role='tool' in session_messages for tool-result persistence
--
-- SQLite does not support ALTER TABLE ... MODIFY COLUMN, so we recreate the
-- table with the updated CHECK constraint and copy existing rows.

CREATE TABLE session_messages_new (
    id         TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role       TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system', 'tool')),
    content    TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO session_messages_new SELECT * FROM session_messages;

DROP TABLE session_messages;

ALTER TABLE session_messages_new RENAME TO session_messages;

CREATE INDEX IF NOT EXISTS idx_session_messages_session_id
    ON session_messages(session_id);
