-- voice_max_turns: 8 -> 0 (no voice-specific cap).
--
-- A spoken request got eight agent-loop turns where a typed one got fifty.
-- Migration 0035 raised `agent_max_turns` from 20 to 50 because "the 20-turn
-- cap stranded multi-step research and home-automation requests mid-task" —
-- and voice, which is where a household actually asks for those, was left on
-- the tightest budget in the pond. The same request that completes when typed
-- gives up six times sooner when spoken, which reads as the assistant being
-- unreliable rather than as a configured trade-off.
--
-- The original reasoning was that every extra turn is another round the user
-- waits through in silence. That is bounded now by things that bound time
-- directly rather than by proxy: `agent_timeout_secs` stops a stalled turn, the
-- thinking tone means the wait is not silent, and a spoken barge-in stops a
-- turn that has gone wrong. Capping steps to bound time also priced a cheap
-- tool round the same as an expensive one.
--
-- 0 disables the voice-specific cap; `effective_max_turns` then returns
-- `agent_max_turns` for voice and text alike. A household that would rather be
-- cut off than wait can set a number back, and it is still clamped to
-- `agent_max_turns`.
--
-- Guarded on the stored value and on `is_user_set`, per migration 0035 — anyone
-- who chose 8 deliberately keeps it, and re-running this file is a no-op. The
-- registry entry is `DEFAULT_ADOPTIONS` in
-- `pond-core/src/user_data/domain/settings.rs`.
UPDATE settings
   SET value = '0', updated_at = datetime('now')
 WHERE key = 'voice_max_turns'
   AND value = '8'
   AND is_user_set = 0;
