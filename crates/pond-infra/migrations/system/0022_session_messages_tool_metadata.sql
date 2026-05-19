-- Add tool-call metadata to session_messages and fix the missing 'tool' role.
--
-- Two changes:
--   1. The original CHECK constraint omitted 'tool', so any attempt to persist
--      Role::Tool messages would fail. SQLite cannot ALTER a CHECK constraint
--      in-place, so we rebuild the table.
--   2. Add tool_call_id (links a tool result back to the assistant call) and
--      tool_calls_json (JSON-encoded ToolCallRecord array on assistant rows).
--
-- Existing rows are preserved. The 'embedding' BLOB column (added in 0005) is
-- carried forward; new tool metadata columns default to NULL.

CREATE TABLE session_messages_new (
    id              TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role            TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system', 'tool')),
    content         TEXT NOT NULL,
    embedding       BLOB,
    tool_call_id    TEXT,
    tool_calls_json TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO session_messages_new (id, session_id, role, content, embedding, created_at)
    SELECT id, session_id, role, content, embedding, created_at FROM session_messages;

DROP TABLE session_messages;
ALTER TABLE session_messages_new RENAME TO session_messages;

CREATE INDEX IF NOT EXISTS idx_session_messages_session_id
    ON session_messages(session_id);
CREATE INDEX IF NOT EXISTS idx_session_messages_created_at
    ON session_messages(created_at ASC);
