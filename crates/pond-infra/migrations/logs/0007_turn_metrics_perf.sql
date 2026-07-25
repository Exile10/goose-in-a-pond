-- Per-turn inference performance columns (engine-reported; nullable because
-- HTTP providers without ProviderStats leave them empty).

ALTER TABLE turn_metrics ADD COLUMN prefill_ms           INTEGER;
ALTER TABLE turn_metrics ADD COLUMN model_load_ms        INTEGER;
ALTER TABLE turn_metrics ADD COLUMN decode_tok_per_sec   REAL;
ALTER TABLE turn_metrics ADD COLUMN prefill_tok_per_sec  REAL;
ALTER TABLE turn_metrics ADD COLUMN context_limit_tokens INTEGER;
ALTER TABLE turn_metrics ADD COLUMN inference_count      INTEGER;
