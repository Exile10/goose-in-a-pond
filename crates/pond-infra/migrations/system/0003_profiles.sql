-- Household member profiles

CREATE TABLE IF NOT EXISTS profiles (
    id           TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    avatar_emoji TEXT NOT NULL DEFAULT 'duck',
    preferences  TEXT NOT NULL DEFAULT '{}',
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_profiles_created_at ON profiles(created_at);

ALTER TABLE sessions ADD COLUMN profile_id TEXT REFERENCES profiles(id);
ALTER TABLE sessions ADD COLUMN input_mode TEXT NOT NULL DEFAULT 'rest';
