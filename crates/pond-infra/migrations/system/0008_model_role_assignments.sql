-- Role → model assignments. Source of truth for which model handles each role.
-- The settings KV table holds a hot-cache of the same data, synced at startup.
--
-- Valid roles: "chat" | "think" | "task" | "asr" | "tts"

CREATE TABLE IF NOT EXISTS model_role_assignments (
    role        TEXT PRIMARY KEY,           -- "chat"|"think"|"task"|"asr"|"tts"
    model_id    TEXT NOT NULL,              -- references models(id)
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
