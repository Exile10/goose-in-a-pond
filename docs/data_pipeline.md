# Data Pipeline — Constrained Inference Architecture

## 1. Overview

The data pipeline connects every input source (voice, REST, future sensors/camera) to the LLM and back to storage, while keeping the system healthy on constrained hardware (Jetson Orin Nano, 8–16 GB unified RAM).

Before this work the pipeline had three critical flaws:
- `chat_once()` loaded **all** conversation history on every message with no token budget — after 20–30 turns on a 7B Q4 model the context silently overflowed, dropping the most recent messages
- `max_tokens` was hardcoded to `512` with no way to override it
- Storage grew forever — no pruning, no TTL

This feature implements **P1–P4** of the five-stage pipeline plan.

---

## 2. Features

### P1 — Context Budget Manager
Limits conversation history to a safe character budget before every LLM call.

| Constant | Value | Purpose |
|---|---|---|
| `MAX_CONTEXT_CHARS` | 12 000 | ~3 K tokens at 4 chars/token |
| `RESERVE_FOR_RESPONSE_CHARS` | 2 048 | Reserved for the model's reply |
| `USABLE_HISTORY_CHARS` | 9 952 | Hard cap on history fed to the model |

- `trim_to_budget(messages)` — walks newest-first, keeps messages until the budget is exhausted, reverses to chronological order before returning
- `get_recent_messages(session_id, limit=100)` — new port method that fetches the 100 most recent messages (DESC LIMIT + reverse) instead of the unbounded `get_messages()`
- Single oversized message is truncated rather than silently dropped

### P2 — Provider Config + FallbackProvider
- `LlamafileProvider` and `OllamaProvider` now accept `with_max_tokens(u32)` and `with_temperature(f32)` builders; defaults raised to `max_tokens: 1024`
- `FallbackProvider` — wraps two `Arc<dyn LlmProvider>`: tries primary, on any error transparently retries with fallback
- Chainable: `FallbackProvider::new(llamafile, FallbackProvider::new(ollama, mock))`
- Server runs `FallbackProvider(llamafile → ollama)` automatically; ChatService falls back to `MockAgent` echo if both are down

### P3 — TTL Migrations + Pruning Job
Background task (every 6 hours) prunes all high-frequency tables:

| Table | Retention | Rule |
|---|---|---|
| `event_log` | 30 days | DELETE all older rows |
| `sensor_readings` | 7 days | DELETE all older rows |
| `camera_events` | 14 days | DELETE acknowledged-only (keep unacknowledged alerts) |
| `session_messages` | 500 per session | Keep most recent 500, delete the rest |

New migration files:
- `crates/pond-infra/migrations/logs/0002_sensor_readings.sql`
- `crates/pond-infra/migrations/logs/0003_camera_events.sql`

### P4 — REST Chat with Real LLM
`POST /api/v1/chat` now calls `ChatService::chat_once()` which:
1. Resolves or creates the session
2. Saves the user message
3. Loads budgeted history (`get_recent_messages` + `trim_to_budget`)
4. Calls the configured `LlmProvider`
5. Saves the assistant response
6. Returns the response text

---

## 3. Executor Functions and Models

### New files

| File | Purpose |
|---|---|
| `crates/pond-core/src/services/context_budget.rs` | Budget constants + `trim_to_budget()` |
| `crates/pond-core/src/services/fallback_provider.rs` | `FallbackProvider` struct + `LlmProvider` impl |
| `crates/pond-infra/src/pruning.rs` | `PruningConfig`, `run_pruning()`, `prune_once()` |
| `crates/pond-infra/migrations/logs/0002_sensor_readings.sql` | `sensor_readings` table |
| `crates/pond-infra/migrations/logs/0003_camera_events.sql` | `camera_events` table |

### Modified files

| File | Change |
|---|---|
| `crates/pond-core/src/ports/session_storage.rs` | Added `get_recent_messages()` to trait |
| `crates/pond-core/src/services/mock_session.rs` | Implemented `get_recent_messages()` |
| `crates/pond-core/src/services/chat.rs` | Replaced unbounded load with budget-aware load |
| `crates/pond-core/src/services/mod.rs` | Exposed `context_budget`, `fallback_provider` |
| `crates/pond-infra/src/sqlite_session_storage.rs` | Implemented `get_recent_messages()` (DESC LIMIT + reverse) |
| `crates/pond-infra/src/lib.rs` | Exposed `pruning` module |
| `crates/pond-adapters-llamafile/src/lib.rs` | Added `max_tokens`, `temperature` builders |
| `crates/pond-adapters-ollama/src/lib.rs` | Added `max_tokens`, `temperature` builders |
| `crates/pond-api/src/lib.rs` | Added `agent`, `llm_provider` to `AppState` |
| `crates/pond-api/src/routes.rs` | Wired `ChatService` into REST `/chat` handler |
| `crates/pond-server/src/main.rs` | FallbackProvider chain + pruning task spawn |

### Key types

```rust
// context_budget.rs
pub fn trim_to_budget(messages: Vec<ChatMessage>) -> Vec<ChatMessage>

// fallback_provider.rs
pub struct FallbackProvider { primary, fallback }
impl FallbackProvider {
    pub fn new(primary: Arc<dyn LlmProvider>, fallback: Arc<dyn LlmProvider>) -> Self
}

// pruning.rs
pub struct PruningConfig {
    pub event_log_days: u32,         // default: 30
    pub sensor_readings_days: u32,   // default: 7
    pub camera_events_days: u32,     // default: 14
    pub session_messages_keep: u32,  // default: 500
    pub interval_hours: u64,         // default: 6
}
pub async fn run_pruning(logs: Pool<Sqlite>, system: Pool<Sqlite>, config: PruningConfig)
```

---

## 4. Expected API Calls and Endpoints

### Chat

```
POST /api/v1/chat
Content-Type: application/json

{
  "message": "What is the weather like today?",
  "session_id": "optional-uuid"          // omit to start a new session
}
```

Response:
```json
{
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "response": "I don't have real-time weather data, but..."
}
```

Errors:
- `400 Bad Request` — missing or malformed JSON body
- `403 Forbidden` — onboarding not complete (middleware guard)
- `500 Internal Server Error` — LLM and fallback both failed

### Sessions

```
GET  /api/v1/sessions              // list all sessions (most recent first)
PATCH /api/v1/sessions/:id         // rename a session
```

Rename body:
```json
{ "title": "Morning Weather Chat" }
```

---

## 5. Security / Security Implementations

- **Onboarding guard middleware** — all chat and session routes return `403` until `OnboardingStep::Completed`; health, onboard, system/info remain public
- **Rate limiter** — 100 requests per 60 seconds per client IP, applied at the router level before all routes
- **Context budget** — caps history at 9 952 chars before every LLM call; prevents prompt injection via history accumulation
- **TTL pruning** — removes old data on a schedule; limits exposure of historical sensor/camera data
- **No secrets in state** — LLM endpoints are local (`127.0.0.1`), no API keys stored in `AppState`

---

## 6. Lessons

- **`sqlx::query()` multi-statement silent failure** — SQLx only executes the first statement in a string with multiple SQL statements. Always use `sqlx::migrate!()` for schema changes; never inline multiple `CREATE TABLE` statements in one `query!()` call.
- **`goose` transitive dependency conflict on Windows** — Adding `pond-adapters-goose` to `pond-server` pulls in `pctx_code_execution_runtime-0.1.3` which depends on two incompatible versions of `rmcp` (`0.14` and `1.2`). Never add `pond-adapters-goose` to `pond-server`. Use the pure-HTTP adapters (`pond-adapters-llamafile`, `pond-adapters-ollama`) for the server binary.
- **`datetime('now')` SQLite format** — Produces `"YYYY-MM-DD HH:MM:SS"`. Use `NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")` when reading, NOT `DateTime::parse_from_rfc3339`.
- **DESC LIMIT + reverse for chronological order** — `get_recent_messages()` fetches newest-first with `ORDER BY created_at DESC LIMIT ?` then `.reverse()`s the Vec. This is more efficient than fetching all rows and slicing.
- **`FallbackProvider` requires `Clone` on messages** — `provider.complete(system, messages.clone())` must clone before passing to primary so the same Vec can be passed to fallback on error.

---

## 7. Feature Implementation Conclusion

P1–P4 are complete. The pipeline now:

1. **Bounds context** — no more silent overflow on long conversations
2. **Survives provider failures** — llamafile down → automatic fallback to ollama
3. **Prunes storage** — background task keeps all four high-frequency tables within retention limits
4. **Delivers real LLM responses via REST** — `POST /api/v1/chat` calls the configured provider chain, falls back to mock echo when all providers are unreachable

P5 (pipeline transport channel), P6 (sensor/camera ports), and P7 (streaming) remain deferred until validated on-device.

---

## 8. Implementation Notes

### How to run

**CLI chat (no server, uses mock echo):**
```bash
cargo run -p pond-server -- chat
```

**CLI chat with llamafile (requires llamafile running on port 8080):**
```bash
cargo run -p pond-server -- chat --provider llamafile
```

**CLI chat with Ollama (requires Ollama running on port 11434):**
```bash
cargo run -p pond-server -- chat --provider ollama --model llama3.2
```

**REST server (llamafile → ollama fallback chain):**
```bash
cargo run -p pond-server -- serve [--port 4000]
```

Test the chat endpoint:
```bash
curl -X POST http://localhost:4000/api/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"message": "hello"}'
```

### Success Metrics

| Metric | Target | Verified |
|---|---|---|
| Context stays within budget | ≤ 9 952 chars fed to LLM | ✅ `trim_to_budget` tests |
| History loaded efficiently | Most recent 100 msgs only | ✅ `get_recent_messages` tests |
| Provider failover | Primary fail → fallback called | ✅ `fallback_provider` tests |
| REST chat returns LLM response | Not echo | ✅ Routes wired to `ChatService` |
| TTL pruning deletes old rows | All 4 tables pruned correctly | ✅ `pruning` tests |
| Full test suite passes | 69+ tests green | ✅ `cargo test -p pond-core -p pond-infra -p pond-api` |

### Fast build (skips Goose compilation)
```bash
cargo build -p pond-core -p pond-infra -p pond-api -p pond-server
```
