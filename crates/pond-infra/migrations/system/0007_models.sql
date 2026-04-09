-- Model catalog: persists all known models across all families.
-- Survives restarts; seeded from registry.json at startup.
-- is_custom=1 rows are user-added and never clobbered by registry seeding.

CREATE TABLE IF NOT EXISTS models (
    id               TEXT PRIMARY KEY,        -- "{category}/{name}"
    category         TEXT NOT NULL,           -- "gguf"|"llamafile"|"ollama"|"whisper"|"tts_piper"|"tts_http"
    name             TEXT NOT NULL,

    filename         TEXT,                    -- on-disk filename; NULL for Ollama
    description      TEXT NOT NULL DEFAULT '',
    size_mb          INTEGER NOT NULL DEFAULT 0,

    -- Source / registry
    url              TEXT,                    -- download URL
    hf_id            TEXT,                    -- HuggingFace "owner/repo:quant"

    -- LLM fields (gguf / llamafile / ollama)
    ram_estimate_mb  INTEGER,                 -- runtime RAM needed in MB
    recommended_role TEXT,                    -- "chat"|"think"|"task"
    context_length   INTEGER,                 -- max context tokens
    quantization     TEXT,                    -- "Q4_K_M", "Q5_K_S", etc.

    -- ASR fields (whisper)
    asr_language     TEXT,                    -- "en", "multilingual"
    asr_size         TEXT,                    -- "tiny"|"base"|"small"|"medium"|"large"

    -- TTS fields (tts_piper / tts_http)
    tts_engine       TEXT,                    -- "piper"|"http"|"qwen"
    tts_voice_name   TEXT,                    -- "lessac", "Vivian"
    config_filename  TEXT,                    -- piper: companion .onnx.json
    config_url       TEXT,                    -- piper: config download URL
    tts_url          TEXT,                    -- http: base URL
    sample_rate      INTEGER,                 -- output Hz e.g. 22050

    -- State
    downloaded       INTEGER NOT NULL DEFAULT 0,   -- 1 = file on disk
    is_custom        INTEGER NOT NULL DEFAULT 0,   -- 1 = user-added

    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_models_category_name ON models(category, name);
