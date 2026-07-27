-- Durable pairing between a GIAP session and the agent engine's own session.
--
-- The engine (goose) keeps its conversation in its own store under an id it
-- generates itself. That pairing used to live only in a process-local HashMap,
-- so after a pond-server restart an existing chat resolved to a brand-new EMPTY
-- engine session and the model lost the whole conversation even though every
-- message was still in session_messages.
--
-- Deliberately NOT a column on `sessions` and NOT a foreign key: the pairing is
-- also established from paths that run before a GIAP session row exists
-- (direct tool calls, the voice child). The engine id is opaque here and must be
-- re-validated against the engine before use — the engine's store can be wiped
-- independently of this database.

CREATE TABLE IF NOT EXISTS engine_session_map (
    session_id        TEXT PRIMARY KEY,
    engine_session_id TEXT NOT NULL,
    updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
