-- PAI-5 P2. Tokens the turn spent on reasoning the user never saw.
--
-- Extends 0029's pattern (prompt_tokens / completion_tokens on the assistant
-- row). Nullable with no DEFAULT, deliberately: every row that already exists
-- was written before anything counted, and NULL says "nobody counted" where a
-- DEFAULT 0 would assert the false claim that those turns did no thinking.
-- PAI-5 P5 derives an output reserve from this column and must be able to tell
-- the two apart.
--
-- The count only. The reasoning TEXT is not stored anywhere and is not replayed
-- into context; PAI-5 P6 owns that and has not landed.

ALTER TABLE session_messages ADD COLUMN reasoning_tokens INTEGER;
