-- Settings are a flat key-value table and a factory default only applies when
-- the key is ABSENT. Any install that ever saved a settings snapshot has every
-- key pinned to whatever the default was that day, so every default changed
-- since is invisible there — forever. Two halves fix that.
--
-- 1. `is_user_set` records that the user DELIBERATELY chose a key. A row on its
--    own proves nothing: server-side writers (role-assignment sync, model
--    activation, onboarding) write keys the user never touched. The key set of
--    a `PUT /api/v1/settings` patch does prove it, so that is the only writer
--    that marks. A marked key is exempt from every future default adoption.
--
-- 2. The backlog below adopts the defaults that changed while installs already
--    existed. The rule is narrow on purpose: adopt only where the stored value
--    is still byte-equal to the OLD default, which means the user never chose
--    anything different. Accepted false positive — a user who deliberately
--    picked exactly the old value is indistinguishable from one who never
--    chose, and is moved once; their next explicit save marks the key and ends
--    the ambiguity.
--
--    Two keys on the motivating database are deliberately NOT here, and not for
--    the reason one might guess — both have an explicit stored row, so "absent
--    keys already get the default" does not apply to either:
--      * `tool_selection_mode = 'all'` — the factory default is still 'all'.
--        Nothing moved, so there is nothing to adopt; an entry would be a no-op.
--      * `embedding_provider = 'none'` — this key has defaulted to 'fastembed'
--        since it was introduced, so 'none' was never any default and matches no
--        old value this rule could key on. Reaching it needs a hand flip or a UI
--        prompt, not an adoption.
--
-- The registry of adopted defaults lives in
-- `pond-core/src/user_data/domain/settings.rs` (`DEFAULT_ADOPTIONS`); its test
-- fails if one of these defaults moves again, forcing a NEW migration that
-- chains off this one (`old_default` = the value below) rather than an edit to
-- this file, which has already run on real machines.
-- `every_adoption_entry_has_matching_migration_sql` (pond-infra) ties the two
-- directions together: no entry without a matching UPDATE here, and no UPDATE
-- here without an entry. Both UPDATEs are guarded on the stored value, so
-- re-running this file is a no-op after the first pass.
ALTER TABLE settings ADD COLUMN is_user_set INTEGER NOT NULL DEFAULT 0;

-- agent_max_turns: 20 -> 50. A multi-step research or home-automation request
-- routinely needs more than 20 provider calls and used to be stranded mid-task.
UPDATE settings
   SET value = '50', updated_at = datetime('now')
 WHERE key = 'agent_max_turns'
   AND value = '20'
   AND is_user_set = 0;

-- hybrid_compaction_enabled: false -> true. Off, the only defence against a
-- full context on-device is Goose's reactive LLM auto-compaction, which stalls
-- a turn mid-conversation.
UPDATE settings
   SET value = 'true', updated_at = datetime('now')
 WHERE key = 'hybrid_compaction_enabled'
   AND value = 'false'
   AND is_user_set = 0;
