# GIAP Debug Guide

A practical reference for diagnosing problems across every layer of the stack.

---

## Quick-Start Checks

Run these first whenever something is broken:

```bash
# 1. Does the server start?
cargo run -p pond-server -- status

# 2. Is the REST API alive?
curl http://localhost:4000/api/v1/health

# 3. Are all tests green?
cargo test -p pond-core -p pond-infra -p pond-api

# 4. Fast build (skips Goose compilation, ~25s)
cargo build -p pond-core -p pond-infra -p pond-api -p pond-server
```

---

## Debug Logging

Enable verbose output with the `--debug` flag or the `RUST_LOG` env var:

```bash
# Built-in debug flag (serve command only)
cargo run -p pond-server -- serve --debug

# Fine-grained env var (works for all commands)
RUST_LOG=debug cargo run -p pond-server -- chat --provider llamafile

# Specific crate only (less noise)
RUST_LOG=pond_core=debug,pond_infra=info cargo run -p pond-server -- serve
```

Log levels: `error` -> `warn` -> `info` -> `debug` -> `trace`

---

## Prompt Debugging

### GIAP_DUMP_PROMPT

**File:** `crates/pond-inference/src/provider.rs:168`

Dumps the fully-rendered prompt (after Jinja template application) to a file. This shows exactly what the model receives: system prompt, tool declarations rendered through the chat template, and all conversation messages.

```bash
# Dump to /tmp/giap-rendered-prompt.txt
GIAP_DUMP_PROMPT=1 cargo run -p pond-server -- serve

# Dump to a custom path
GIAP_DUMP_PROMPT=/tmp/debug-prompt.txt cargo run -p pond-server -- serve
```

Inspect the output to verify:
- Tools appear in the correct format for the model (e.g. Gemma 4's `<|tool>declaration:NAME{...}<tool|>`)
- System prompt includes all expected sections (identity, instructions, context-handling)
- User message has the `<system-context>` / `<user-message>` XML wrapper
- Thinking mode configuration is applied

### Tool Schema Logging (MCP Dispatcher)

**File:** `crates/pond-mcp-server/src/dispatcher.rs:274`

Enable debug logging on the MCP server crate to see the full JSON schema for every tool as it is collected from each MCP server:

```bash
RUST_LOG=pond_mcp_server=debug cargo run -p pond-server -- serve
```

Log lines to look for:

```
[DEBUG pond_mcp_server::dispatcher] tool schema — prefix="giap-weather__" tool="get_current_weather" description="..." schema={...}
[INFO  pond_mcp_server::dispatcher] total tool definitions collected from MCP servers — total=35
```

Each tool's full parameter schema is logged, so you can verify the model receives correct parameter definitions (type, description, required fields).

### Agent Tool Decision Logging

**File:** `crates/pond-agent/src/agent.rs:405`

Enable debug logging on the agent crate to see live tool schema fetching decisions:

```bash
RUST_LOG=pond_agent=debug cargo run -p pond-server -- serve
```

Log lines to look for:

```
[DEBUG pond_agent::agent] tool schemas fetched live from dispatcher — tool_calling=true tools_count=35 full_json_len=8234
```

This confirms:
- Whether tool calling is enabled (based on model capabilities)
- How many tools were passed to the provider
- The size of the pre-formatted JSON override

### Inference Engine Logging

**File:** `crates/pond-inference/src/provider.rs`

Enable debug logging on the inference crate to trace the full generation pipeline:

```bash
RUST_LOG=pond_inference=debug cargo run -p pond-server -- serve
```

Key log lines at each stage:

| Stage | Log line pattern | What it tells you |
|-------|-----------------|-------------------|
| Template | `template applied -- tools_count=N has_tools_json=true` | Tools were included in the template |
| Prompt tail | `prompt tail (last 500 chars)` | Whether tool declarations appear at the end |
| KV cache | `KV cache partial hit (in-memory) -- tokens_saved=N` | How many tokens were reused |
| Raw output | `raw model output (first 300 chars)` | What the model actually generated |
| Tool parse | `generation complete, parsing tool calls` | Whether tool calls were detected |

### Combined Debug Session

For a full diagnostic session showing the complete flow from prompt to tool dispatch:

```bash
GIAP_DUMP_PROMPT=1 \
RUST_LOG=pond_agent=debug,pond_inference=debug,pond_mcp_server=debug \
  cargo run -p pond-server -- serve
```

---

## Workflow Loop Tracing

The state machine logs each transition at `DEBUG` level:

```
[DEBUG pond_core::shared::services::chat] Workflow state: Wait
[DEBUG pond_core::shared::services::chat] Workflow state: Listen
[DEBUG pond_core::shared::services::chat] User input: "hello world"
[DEBUG pond_core::shared::services::chat] Workflow state: Thinking
[DEBUG pond_core::shared::services::chat] Workflow state: Speak
[DEBUG pond_core::shared::services::chat] Agent output: "Echo: hello world"
```

If you see `Wait` but never `Listen`, the wake word detector is not activating.
If you see `Listen` but no `Thinking`, the voice input returned `None` (EOF / empty).

---

## Provider Debugging

### Check which provider is active

```bash
# See the fallback chain in logs
RUST_LOG=pond_core::models::services::fallback_provider=debug cargo run -p pond-server -- serve
```

Fallback log lines look like:
```
[WARN  pond_core::models::services::fallback_provider] Primary provider 'LLaMA_CPP' failed (...), falling back to 'llama3.2'
```

### llamafile not responding

```bash
# Is it running?
curl http://127.0.0.1:8080/v1/models

# Test a direct completion
curl -X POST http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"LLaMA_CPP","messages":[{"role":"user","content":"hi"}],"max_tokens":50}'
```

Errors:
- `connection refused` → llamafile not started; run `./your-model.llamafile`
- `model not found` → wrong model name; llamafile always reports `"LLaMA_CPP"`

### Ollama not responding

```bash
# Is it running?
curl http://127.0.0.1:11434/api/tags

# Test a direct completion
curl -X POST http://127.0.0.1:11434/api/chat \
  -H "Content-Type: application/json" \
  -d '{"model":"llama3.2","messages":[{"role":"user","content":"hi"}]}'
```

Errors:
- `connection refused` → run `ollama serve`
- `model not found` → run `ollama pull llama3.2`

---

## Voice / Whisper Debugging

### Is whisper.cpp running?

```bash
# Health check
curl http://127.0.0.1:9000/

# Test with a WAV file
curl -X POST http://127.0.0.1:9000/inference \
  -F "file=@tests/blobs/jfk.wav" \
  -F "response_format=json"
```

Expected response: `{"text":" And so my fellow Americans ask not what your country can do for you..."}`

Errors:
- `connection refused` → start whisper.cpp: `./server -m models/ggml-base.en.bin --port 9000`
- `500` error → model file missing or corrupt; re-run `cargo run -p pond-server -- setup`

### No audio captured

```bash
# List audio devices
RUST_LOG=debug cargo run -p pond-server -- chat --voice 2>&1 | grep -i audio
```

Common causes:
- No default microphone configured (headless server)
- cpal on Windows: device may report `I16` format; the adapter handles F32/I16/U16
- Audio recorded but whisper returns empty string → recording too short / silent environment

### Wake word not triggering

The `WhisperKeywordDetector` polls in 2-second cycles. Expected log flow:
```
[DEBUG pond_adapters_whisper] No wake word, polling again...
[DEBUG pond_adapters_whisper] No wake word, polling again...
[INFO  pond_adapters_whisper] Wake word detected: "Hey goose, how are you"
```

Debugging:
- Is whisper returning any transcript? Add a temporary `println!` or check with `--debug`
- Is the trigger word spelled correctly? The match is case-insensitive substring (`"goose"` matches `"Goose"`, `"Hey Goose"`)
- Try a shorter trigger like `"go"` to confirm detection pipeline works

---

## REST API Debugging

### Onboarding gate (403 Forbidden)

Protected routes return `403` before onboarding is complete:
```bash
# Check onboarding state
curl http://localhost:4000/api/v1/onboard/status

# Run the onboarding wizard
cargo run -p pond-server -- onboard

# Or reset and redo it
cargo run -p pond-server -- onboard --reset
```

### Chat endpoint

```bash
# Basic chat (mock echo, no LLM needed)
curl -X POST http://localhost:4000/api/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"message": "hello"}'

# Continuing a conversation
curl -X POST http://localhost:4000/api/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"message": "remember my name is Jerry", "session_id": "SESSION_ID_FROM_PREVIOUS_RESPONSE"}'

# List sessions
curl http://localhost:4000/api/v1/sessions
```

### Rate limit hit (429)

The limiter allows 100 requests / 60 seconds per IP. If you're hammering the API in tests, add a `tokio::time::sleep` or use the mock router directly (no rate limiter in unit tests).

---

## Database Debugging

### Find the database files

```
Windows: %APPDATA%\goose-in-a-pond\pond_system.db
         %APPDATA%\goose-in-a-pond\pond_logs.db
Linux:   ~/.local/share/goose-in-a-pond/pond_system.db
```

### Inspect with sqlite3

```bash
sqlite3 "%APPDATA%\goose-in-a-pond\pond_system.db"

# Check schema
.schema

# Check onboarding state
SELECT * FROM onboarding_state;

# Check sessions
SELECT id, title, created_at, updated_at FROM sessions ORDER BY updated_at DESC LIMIT 10;

# Count messages in a session
SELECT session_id, COUNT(*) as msg_count FROM session_messages GROUP BY session_id;

# Check pruning: rows older than 30 days in event_log
SELECT COUNT(*) FROM event_log WHERE timestamp < datetime('now', '-30 days');
```

### Reset the database (nuclear option)

```bash
# Windows
rm "$env:APPDATA\goose-in-a-pond\pond_system.db"
rm "$env:APPDATA\goose-in-a-pond\pond_logs.db"

# Then re-run setup to reinitialise
cargo run -p pond-server -- setup
```

### Migration won't apply

SQLx migrations run at startup via `sqlx::migrate!()`. If a migration fails:
1. Check for syntax errors in `crates/pond-infra/migrations/`
2. Check the `_sqlx_migrations` table to see which migrations have run
3. **Never** edit an already-applied migration file — create a new migration instead

---

## Build Debugging

### Slow build

The full workspace includes the Goose submodule (~10 min first compile). Use the fast path:
```bash
cargo build -p pond-core -p pond-infra -p pond-api -p pond-server
```

### `pond-adapters-goose` transitive dep conflict

Adding `pond-adapters-goose` to `pond-server` breaks the Windows build because `pctx_code_execution_runtime-0.1.3` uses two incompatible versions of `rmcp`. **Never add `pond-adapters-goose` as a direct dependency of `pond-server`.**

Error signature:
```
error[E0308]: mismatched types
  --> pctx_code_execution_runtime-0.1.3/src/mcp_registry.rs
note: two different versions of crate `rmcp` are being used
```

Fix: remove `pond-adapters-goose` and `goose` from `pond-server/Cargo.toml`. Use `pond-adapters-llamafile` and `pond-adapters-ollama` (pure HTTP, no Goose dep) instead.

### `sqlx` compile error — migration file not found

```
error: migration file not found
```

Cause: `sqlx::migrate!("migrations/system")` is a compile-time macro. The migration directory is resolved relative to the **crate's** `Cargo.toml`, not the workspace root. Always create the `.sql` file on disk before recompiling.

---

## Test Debugging

### Run a single test

```bash
cargo test -p pond-core -- chat_once_returns_echo
cargo test -p pond-infra -- get_recent_messages
cargo test -p pond-api --test onboarding_integration_test -- chat_is_blocked_before_onboarding
```

### Run with output (see `println!` inside tests)

```bash
cargo test -p pond-core -- --nocapture
```

### Integration tests need a temp database

`onboarding_integration_test.rs` uses `tempfile::TempDir` + a real SQLite DB. If tests fail with database errors, check that `tempfile` is in `[dev-dependencies]`.

### Context budget test

```bash
cargo test -p pond-core -- context_budget
```

Verifies:
- Empty input → empty output
- Small history passes through unchanged
- Large history trimmed to ≤ `USABLE_HISTORY_CHARS` (9 952 chars)
- Most recent messages are always preserved
- Single oversized message is truncated (not dropped)

---

## Common Error Messages

| Error | Cause | Fix |
|---|---|---|
| `connection refused 127.0.0.1:8080` | llamafile not running | Start llamafile |
| `connection refused 127.0.0.1:11434` | Ollama not running | `ollama serve` |
| `connection refused 127.0.0.1:9000` | whisper.cpp not running | Start whisper server |
| `403 Forbidden` on `/api/v1/chat` | Onboarding incomplete | Run `pond-server onboard` |
| `400 Bad Request` on `/api/v1/chat` | Missing or invalid JSON body | Add `Content-Type: application/json` header |
| `No audio input device found` | No microphone / headless server | Drop `--voice` and type instead |
| `whisper returned empty text` | Silence / too-short recording | Speak louder, check mic, or increase `--duration` |
| `could not compile pctx_code_execution_runtime` | goose dep added to pond-server | Remove `pond-adapters-goose` from pond-server deps |
| `migration file not found` at compile time | SQL file created after last compile | Create `.sql` file, then `cargo build` |

---

## Model Management Debugging

### List the catalog

```bash
cargo run -p pond-server -- models list
cargo run -p pond-server -- models list --category gguf
cargo run -p pond-server -- models list --downloaded
```

### Check role assignments

```bash
cargo run -p pond-server -- models list-assignments
```

### Assign a model to a role

```bash
cargo run -p pond-server -- models assign --role chat --model gguf/llama-3b
cargo run -p pond-server -- models assign --role asr  --model whisper/base
cargo run -p pond-server -- models assign --role tts  --model tts_piper/en-lessac
```

### Re-seed the catalog (if DB is empty or URL changed)

```bash
# Happens automatically on `pond-server serve`, or force via:
cargo run -p pond-server -- setup
```

### Check downloaded flag vs actual files

```bash
# The 'downloaded' flag is set from disk at startup. If it's wrong, restart the server.
# Files should be at:
#   Whisper:   $DATA_DIR/models/ggml-*.bin
#   Llamafile: $DATA_DIR/models/llm/*.llamafile
#   GGUF:      $DATA_DIR/models/gguf/*.gguf
#   TTS:       $DATA_DIR/models/tts/*.onnx
#   Binaries:  $DATA_DIR/bin/whisper-server, $DATA_DIR/bin/piper
sqlite3 "$DATA_DIR/pond_system.db" "SELECT id, downloaded FROM models ORDER BY id;"
```

---

## Phase Status at a Glance

| Phase | Status | Test command |
|---|---|---|
| Foundation + Infrastructure | Done | `cargo test -p pond-core -p pond-infra` |
| Think (llamafile / ollama / gguf) | Done | `cargo run -- chat --provider llamafile` |
| Session persistence | Done | `cargo test -p pond-infra -- sqlite_session` |
| Listen (Whisper ASR) | Done | `cargo run -- chat --voice` |
| Data pipeline (P1–P4) | Done | `cargo test -p pond-core -- context_budget` |
| Wait (Wake word) | Done | `cargo run -- chat --voice` (says "goose") |
| Speak (TTS — Piper) | Done | `cargo run -- chat --tts piper` |
| Model catalog + roles (DB-driven) | Done | `cargo test -p pond-api --test model_integration_test` |
| Goose agent + MCP extensions | Done | `cargo run -- serve` + trigger agent |
| Deployment (Jetson ARM64) | Pending | — |
