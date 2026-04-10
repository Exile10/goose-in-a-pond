CREATE TABLE IF NOT EXISTS user_skills (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    content    TEXT NOT NULL,
    active     INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
