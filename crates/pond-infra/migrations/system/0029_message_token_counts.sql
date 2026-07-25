-- Real per-message token counts (assistant rows carry the turn's prompt +
-- completion sizes from the engine; user/tool rows stay NULL).

ALTER TABLE session_messages ADD COLUMN prompt_tokens     INTEGER;
ALTER TABLE session_messages ADD COLUMN completion_tokens INTEGER;
