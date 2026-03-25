-- Add session title and performance indexes

ALTER TABLE sessions ADD COLUMN title TEXT;

CREATE INDEX IF NOT EXISTS idx_sessions_updated_at ON sessions(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_session_messages_created_at ON session_messages(created_at ASC);
