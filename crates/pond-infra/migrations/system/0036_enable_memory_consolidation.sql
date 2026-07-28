-- memory_consolidation_enabled: false -> true.
--
-- Extraction is imperfect by construction — a 4B model deciding, in the
-- background, which sentences are durable facts about its user. Consolidation
-- is the only thing that ever removes what extraction gets wrong, and it was
-- shipped switched off, so on a real device nothing had ever pruned anything.
--
-- That store audited at 87% noise: the assistant's own self-description filed
-- under the user's identity ("I am a computer program designed to provide
-- information and assist with tasks"), five third-party biography facts in the
-- identity segment, a clock reading kept as a permanent memory, and the
-- extractor's own refusal prose ("No relevant facts about the user were
-- provided in this conversation turn") stored as a fact. The consolidation
-- prompt already names every one of those categories as prunable. It had simply
-- never been allowed to run.
--
-- Same narrow rule as 0035: adopt only where the stored value is still the OLD
-- default, which is the only evidence that nobody chose it. A key the user set
-- deliberately (is_user_set = 1) is left alone.
--
-- Cost is bounded and idle-only: one LLM call per interval (default 24h),
-- triggered by inactivity and cancelled the moment a turn arrives.
UPDATE settings
   SET value = 'true', updated_at = datetime('now')
 WHERE key = 'memory_consolidation_enabled'
   AND value = 'false'
   AND is_user_set = 0;
