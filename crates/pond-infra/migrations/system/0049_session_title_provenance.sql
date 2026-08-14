-- Who last named a conversation, and how far that name reaches.
--
-- `sessions.title` has had exactly one writer since it existed: the
-- deterministic six-word fallback, set once when the first user message lands
-- and never revisited. The idle re-titling job adds two more writers -- a
-- model, and a person using the rename endpoint -- and the three have to be
-- told apart, because overwriting a name someone chose has no undo.
--
-- `title_source` is one of 'derived', 'model', 'user'.
-- `title_through_message_id` is the newest message a 'model' title covers, so
-- the job can tell a conversation that has moved on from one that has not.
--
-- Both are deliberately left NULL on existing rows rather than backfilled to
-- 'derived'. A backfill would have to assume nobody has ever renamed a
-- conversation by hand, and every row where that assumption was wrong would be
-- silently overwritten on the job's first pass. NULL means "unknown", and the
-- gate treats unknown as off-limits unless the stored title is byte-identical
-- to what the fallback would have written -- the one case where we can prove
-- no person composed it.

ALTER TABLE sessions ADD COLUMN title_source TEXT;
ALTER TABLE sessions ADD COLUMN title_through_message_id TEXT;
