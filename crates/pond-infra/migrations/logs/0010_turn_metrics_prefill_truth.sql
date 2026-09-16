-- What prefill actually cost, as opposed to what the prompt happened to be.
--
-- `prefill_tok_per_sec` has been recorded since 0007 and has never been a rate.
-- It was computed as `prompt_tokens / prefill_ms`: the FINAL inference's prompt
-- over EVERY inference's prefill time. Two independent errors that do not
-- cancel, and on an Orin running gemma-4-E4B on 2026-09-10 they read:
--
--   * 3,940 tok/s on a turn whose engine log says `ReusePrefix(7581)` of a
--     7,636-token prompt -- 55 tokens were decoded, not 7,636.
--   * 220 tok/s on a cold turn with four inferences, where 6,807 tokens were
--     decoded once and the other three inferences reused them.
--
-- So the column is wrong by roughly an order of magnitude in both directions,
-- and which direction depends on cache state. Every historical row carries it.
-- There is no backfill: the inputs were never stored, which is what these two
-- columns fix.
--
-- Both nullable with NO DEFAULT, matching 0008. `prefilled_tokens = 0` is a
-- real measurement -- a turn that decoded nothing because the whole prompt was
-- cached -- and is a different fact from NULL, which is every row written
-- before this migration and every row from a provider that reports no
-- ProviderStats. `reused_prefix_tokens = 0` from a backend that DOES report
-- reuse is the interesting one: on turn 2+ it means the prefix stopped being
-- token-stable, which costs a full re-prefill and is otherwise invisible.
--
-- Two ALTERs on an existing table: historical rows keep their data and take
-- NULL for the new columns.

ALTER TABLE turn_metrics ADD COLUMN prefilled_tokens INTEGER;

ALTER TABLE turn_metrics ADD COLUMN reused_prefix_tokens INTEGER;
