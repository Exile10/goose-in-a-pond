-- Which tool GROUPS (MCP extension names) are loaded for a session.
--
-- Phase D2 narrows the tool schemas that reach the model: a small always-on core
-- plus the extension groups scored relevant to the session's opening message,
-- chosen ONCE per session so the local engine's KV prompt prefix stays reusable
-- across turns (per-turn churn would rewrite the tools JSON every turn and
-- defeat the whole point).
--
-- Persisted rather than process-local for one specific reason: the model can
-- widen its own surface mid-session through giap-toolkit's enable_tool_group. An
-- in-memory-only record would silently drop that capability on the next restart,
-- mid-conversation, with no signal to the user about why the assistant suddenly
-- could not do the thing it just did.
--
-- Like engine_session_map (0032), deliberately NOT a column on `sessions` and
-- NOT a foreign key: selection also runs on paths where a GIAP `sessions` row
-- does not exist yet (direct tool calls, the voice child).
--
-- `groups` is a newline-separated list of extension names. A list, not a table:
-- it is always read and written whole, never queried by member, and the whole
-- point of the row is to be one cheap lookup on the turn hot path.

CREATE TABLE IF NOT EXISTS session_tool_groups (
    session_id TEXT PRIMARY KEY,
    groups     TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
