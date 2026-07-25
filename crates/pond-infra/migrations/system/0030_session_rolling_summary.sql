-- Rolling conversation summary, refreshed in idle time by
-- SessionSummaryService and spliced into the model's history by the
-- deterministic turn trimmer as a <conversation-summary> block.

ALTER TABLE sessions ADD COLUMN rolling_summary            TEXT;
ALTER TABLE sessions ADD COLUMN rolling_summary_through_id TEXT;
ALTER TABLE sessions ADD COLUMN rolling_summary_updated_at TEXT;
