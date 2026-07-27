-- A user edit to a system prompt template must survive restarts: the server
-- reseeds factory content for system templates at every boot, which used to
-- overwrite user edits unconditionally. Rows with is_customized = 1 are
-- skipped by the reseed and only return to factory content via an explicit
-- reset (which clears the flag).
ALTER TABLE prompt_templates ADD COLUMN is_customized INTEGER NOT NULL DEFAULT 0;
