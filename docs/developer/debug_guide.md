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

Log levels: `error` → `warn` → `info` → `debug` → `trace`

---

## Workflow Loop Tracing

The state machine logs each transition at `DEBUG` level:

```
[DEBUG pond_core::services::chat] Workflow state: Wait
[DEBUG pond_core::services::chat] Workflow state: Listen
[DEBUG pond_core::services::chat] User input: "hello world"
[DEBUG pond_core::services::chat] Workflow state: Thinking
[DEBUG pond_core::services::chat] Workflow state: Speak
[DEBUG pond_core::services::chat] Agent output: "Echo: hello world"
```

If you see `Wait` but never `Listen`, the wake word detector is not activating.
If you see `Listen` but no `Thinking`, the voice input returned `None` (EOF / empty).

---

## Provider Debugging

### Check which provider is active

```bash
# See the fallback chain in logs
RUST_LOG=pond_core::services::fallback_provider=debug cargo run -p pond-server -- serve
```

Fallback log lines look like:
```
[WARN  pond_core::services::fallback_provider] Primary provider 'LLaMA_CPP' failed (...), falling back to 'llama3.2'
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
RUST_LOG=debug cargo run -p pond-server -- chat --input whisper 2>&1 | grep -i audio
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
| `No audio input device found` | No microphone / headless server | Use `--input stdin` instead |
| `whisper returned empty text` | Silence / too-short recording | Speak louder, check mic, or increase `--duration` |
| `could not compile pctx_code_execution_runtime` | goose dep added to pond-server | Remove `pond-adapters-goose` from pond-server deps |
| `migration file not found` at compile time | SQL file created after last compile | Create `.sql` file, then `cargo build` |

---

## Phase Status at a Glance

| Phase | Status | Test command |
|---|---|---|
| Foundation + Infrastructure | Done | `cargo test -p pond-core -p pond-infra` |
| Think (llamafile / ollama) | Done | `cargo run -- chat --provider llamafile` |
| Session persistence | Done | `cargo test -p pond-infra -- sqlite_session` |
| Listen (Whisper ASR) | Done | `cargo run -- chat --input whisper` |
| Data pipeline (P1–P4) | Done | `cargo test -p pond-core -- context_budget` |
| Wait (Wake word) | Done | `cargo run -- chat --input whisper` (says "goose") |
| Speak (TTS) | Pending | — |
| Deployment (Jetson ARM64) | Pending | — |
