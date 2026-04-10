CREATE TABLE IF NOT EXISTS prompt_templates (
    name        TEXT PRIMARY KEY,
    content     TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    is_system   INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
