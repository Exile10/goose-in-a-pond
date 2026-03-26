-- Memory fragments for semantic/recency-based retrieval

CREATE TABLE IF NOT EXISTS memory_fragments (
    id         TEXT    PRIMARY KEY,
    profile_id TEXT    REFERENCES profiles(id) ON DELETE CASCADE,
    session_id TEXT    REFERENCES sessions(id)  ON DELETE SET NULL,
    content    TEXT    NOT NULL,
    embedding  BLOB,
    source     TEXT    NOT NULL DEFAULT 'chat',
    tags       TEXT    NOT NULL DEFAULT '[]',
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_memory_fragments_profile_id ON memory_fragments(profile_id);
CREATE INDEX IF NOT EXISTS idx_memory_fragments_created_at ON memory_fragments(created_at);

ALTER TABLE session_messages ADD COLUMN embedding BLOB;
