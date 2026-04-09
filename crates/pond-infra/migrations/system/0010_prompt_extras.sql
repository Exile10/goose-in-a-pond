CREATE TABLE IF NOT EXISTS prompt_extras (
    key         TEXT PRIMARY KEY,
    instruction TEXT NOT NULL,
    active      INTEGER NOT NULL DEFAULT 1,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
