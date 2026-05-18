-- Draft staging table for destructive action confirmation flow.
-- Drafts are created by the LLM via the save_draft MCP tool and
-- approved/rejected by the user before execution.
CREATE TABLE IF NOT EXISTS drafts (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    summary TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_drafts_session_status ON drafts(session_id, status);
