-- Per-message training-feedback signal: NULL = no vote, 1 = liked (keep as
-- training data), 0 = disliked (excluded from training data).

ALTER TABLE session_messages ADD COLUMN liked INTEGER;
