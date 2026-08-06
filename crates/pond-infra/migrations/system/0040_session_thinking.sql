-- PAI-5 P6. The reasoning TEXT, stored beside the turn that produced it.
--
-- 0039 stored the COUNT and said out loud that the text was not stored
-- anywhere. This is the other half: the `<thinking>` passages the UI already
-- streams live are, until now, thrown away the instant the SSE connection
-- closes. Reload the page and the thinking panel is empty on every historical
-- turn while the answer it produced is still there.
--
-- A SEPARATE TABLE, not a column on session_messages, for one reason that
-- decides the whole design: this text must never be replayed into context.
-- Everything that builds a prompt reads `session_messages` -- the trimmer, the
-- rolling summariser, the prompt builder. A `thinking` column on that table
-- would ride along in every `SELECT ... FROM session_messages` those paths
-- already run, and the first person to write `SELECT *` would silently feed a
-- model its own discarded scratch work. Putting it in a table nothing but the
-- history-read joins against makes the replay path something you have to write
-- on purpose, and `crates/pond-core/tests/thinking_is_never_replayed.rs`
-- fails the build when somebody does.
--
-- Written only when `settings.persist_thinking` is true, which is FALSE by
-- default. This is reasoning a user was never shown in final form; it is the
-- most candid text the model produces and the least reviewed. Opting in is the
-- correct polarity for a privacy-first pond.
--
-- Applies against a populated database with no backfill: a new table with no
-- DEFAULT and no ALTER cannot disturb a row that already exists. Historical
-- turns simply have no thinking rows, which is the truth about them.
--
-- Scope rides `sessions.profile_id` (0003, indexed in 0037). There is
-- deliberately NO profile_id column here: `session_messages` does not carry one
-- either, and a table whose scope is set by whoever inserts reproduces the
-- `ProfileScope::Owner` no-op exactly -- a column every fixture filled in by
-- hand and no production path ever wrote.

CREATE TABLE IF NOT EXISTS session_thinking (
    id          TEXT PRIMARY KEY,
    -- The assistant row this reasoning produced. CASCADE so deleting a session
    -- (which cascades session_messages) cannot leave orphaned reasoning behind
    -- -- the one kind of orphan that would outlive the conversation a user
    -- asked to forget.
    message_id  TEXT NOT NULL REFERENCES session_messages(id) ON DELETE CASCADE,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    -- Position within the turn, 0-based. A turn emits several passages and
    -- their order is the only thing that makes them readable.
    block_index INTEGER NOT NULL,
    content     TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The one read this table has: "the reasoning for this conversation, grouped by
-- message". One query per history page, same shape as message_attachments.
CREATE INDEX IF NOT EXISTS idx_session_thinking_session
    ON session_thinking(session_id, message_id, block_index);
