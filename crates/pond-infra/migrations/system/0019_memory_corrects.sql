-- Adds correction metadata to memory fragments.
-- For "correction" segment memories, records what wrong claim was corrected
-- so consolidation never accidentally reverts the fix.
ALTER TABLE memory_fragments ADD COLUMN corrects TEXT;
