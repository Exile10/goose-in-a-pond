-- What thinking actually costs, per turn, and what it costs when it goes wrong.
--
-- `turn_metrics` recorded prompt, completion, ttft, latency, tool and context
-- and nothing about reasoning. So on 2026-08-12 nobody could answer either of
-- the two questions that matter about it on-device:
--
--   * How many tokens does thinking spend? Measured by hand at 76-156 per turn,
--     decoded at ~46 tok/s on a Mac and ~25 on an Orin -- one to five seconds of
--     every reply, invisible to the user because `show_thinking` ships false.
--
--   * How often does a turn produce ONLY reasoning and then end? gemma-4-E2B
--     does this reliably for certain phrasings (5/5 on one query, per the fork
--     patch that made goose treat a thinking-only turn as empty). GIAP recovers
--     with EMPTY_TURN_STEER and a second attempt -- a whole extra turn, paid at
--     full price. Nothing counted it, so the true cost of thinking was unknown
--     and could easily exceed the reasoning tokens themselves.
--
-- Both nullable and both with NO DEFAULT, deliberately. `reasoning_tokens = 0`
-- means a turn that did not reason; NULL means nobody counted, which is every
-- row written before this migration and every row from a provider that reports
-- no ProviderStats. Migration 0039 on the system database made exactly this
-- distinction for the same column and for the same reason: a zero from an
-- unmeasured turn is evidence that does not exist, and it will be averaged into
-- any conclusion drawn from this table.
--
-- Against a database that already has rows: two ALTERs on an existing table, so
-- every historical row keeps its data and gets NULL for the new columns. No
-- backfill is possible and none would be honest.

ALTER TABLE turn_metrics ADD COLUMN reasoning_tokens INTEGER;

-- Attempts BEYOND the first, so 0 is the ordinary turn and 1 means the pond
-- went silent once and had to be steered. Not a boolean: the re-engagement
-- budget is a count, and "how often twice" is a different question from "how
-- often at all".
ALTER TABLE turn_metrics ADD COLUMN reengagements INTEGER;
