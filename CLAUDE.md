# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

---

## Terminology & Workflow Conventions

- **"UI"** always means **Pond Desktop** (the Tauri 2.0 desktop app in `pond-desktop/`)
- **"voice mode"** means **both** the CLI voice mode (`pond-server chat --input whisper`) **and** the desktop voice mode (mic orb in Pond Desktop)
- **Proposal reference**: `.ai/proposal.txt` describes the project vision and quarterly milestones — consult it when making architectural decisions
- **Before implementing changes, fixes, or features**: do deep research on the query first (use web search if needed), then write your plan to `.ai/scratchpad.md` before coding

---

## First-Time Installation

```bash
# One-command setup: detects platform, installs everything
bash scripts/install.sh

# Options:
bash scripts/install.sh --desktop        # also build desktop app
bash scripts/install.sh --production     # Linux: systemd + mDNS + auto-start
bash scripts/install.sh --jetson         # Jetson Orin Nano (CUDA auto-detect)
bash scripts/install.sh --minimal        # fastest: server + DB only, no models
bash scripts/install.sh --ollama         # force Ollama as LLM provider
bash scripts/install.sh --llamafile      # force llamafile as LLM provider
bash scripts/install.sh --no-models      # skip all model downloads
bash scripts/install.sh --full           # build entire workspace (10+ min)
```

## Build Commands

```bash
# Fast server build (skips Goose recompile, ~2s)
cargo build -p pond-core -p pond-infra -p pond-api -p pond-server

# Full workspace (compiles Goose submodule — 10+ min first time)
cargo build --workspace

# Cross-compile for Jetson Orin Nano (requires `cargo install cross` + Docker)
SQLX_OFFLINE=true make server       # CPU-only
# CUDA: build natively on the Jetson — see Makefile: `make server-cuda`
```

## Test Commands

```bash
# Rust — single crate (fastest iteration loop)
cargo test -p pond-core
cargo test -p pond-api
cargo test -p pond-adapters-ollama

# Run a single test by name
cargo test -p pond-core -- classify_request::tests::think_keywords_route_to_think

# Live integration tests (require real hardware/services — #[ignore] by default)
GIAP_LLAMAFILE_URL=http://127.0.0.1:8080 \
  cargo test -p pond-server --test live_provider_test -- --ignored llamafile

GIAP_OLLAMA_URL=http://127.0.0.1:11434 GIAP_OLLAMA_MODEL=gemma3:4b \
  cargo test -p pond-server --test live_provider_test -- --ignored ollama

# Frontend unit tests (vitest, happy-dom, no Tauri required)
cd pond-desktop && npm test          # vitest run
cd pond-desktop && npx vitest --watch  # watch mode

# Playwright E2E tests (auto-starts Vite dev server, all API calls mocked)
cd pond-desktop && npx playwright test tests/e2e/
cd pond-desktop && npx playwright test tests/e2e/chat.spec.ts --ui

# Live E2E against real pond-server (optional, skipped when var unset)
GIAP_SERVER_URL=http://127.0.0.1:4000 \
  cd pond-desktop && npx playwright test tests/e2e/
```

## Run the Server

```bash
# First-time setup (downloads Whisper ASR model)
cargo run -p pond-server -- setup

# Serve (REST API on port 4000, web dashboard at http://localhost:4000)
cargo run -p pond-server -- serve

# With native Tauri desktop app
cargo run -p pond-server -- serve --native

# Desktop dev mode (starts Vite + pond-server together)
cd pond-desktop && npm run dev
# OR just Tauri
cd pond-desktop && npm run tauri dev
```

## Lint & Format

```bash
cargo fmt
cargo clippy
```

---

## Architecture

GIAP uses **hexagonal (ports & adapters)** architecture. The rule: `pond-core` never imports from Goose, SQLx, Axum, or any external framework. All I/O crosses a port trait.

```
pond-server  (binary — wires adapters into AppState, starts Axum + Tauri)
    │
pond-api     (Axum HTTP router, AppState struct, SSE streaming, REST handlers)
    │
pond-core    (pure Rust domain — no external deps)
  ├── domain/    ChatMessage, Device, Schedule, MemoryFragment, Settings, ModelCapabilities, ImageAttachment …
  ├── ports/     async_trait interfaces — LlmProvider, Agent, VoiceInput, ToolAgent, AnswerReviewer …
  └── services/  ChatService, request_classifier, context_budget, mock impls
    │
pond-infra           (SQLite via SQLx — two databases)
pond-infra-scheduler (tokio-cron-scheduler adapter)
pond-adapters-*      (one crate per external capability)
pond-desktop         (Tauri 2 + React 19 + Vite — the GUI)
```

### Adding a new capability

Follow the 5-step pattern (documented in `docs/creating-ports-and-adapters.md`):
1. **Domain type** in `crates/pond-core/src/domain/<name>.rs` — pure Rust, no external imports
2. **Port trait** in `crates/pond-core/src/ports/<name>.rs` — `#[async_trait]` interface
3. **Mock + tests** in `crates/pond-core/src/services/` — must pass before any real adapter
4. **Real adapter** as `crates/pond-adapters-<name>/` — implements the port trait
5. **Wire** in `crates/pond-server/src/main.rs` — inject into `AppState`

### Agent Pipeline (Multi-Threaded)

Every chat message flows through a parallelized pipeline:

```
Phase 1 — Parallel Prep (no LLM, <50ms):
  ├── route_to_tool(message)           // keyword routing, instant
  ├── persist user message             // DB write, ~1ms
  └── llamafile readiness check        // HTTP ping (if needed)

Phase 2 — Tool Execution (HTTP/DB only, 100-500ms):
  └── try_tool_agent(tool, message)    // weather API, wikipedia, DB query

Phase 3 — Main Chat Stream (5-30s):
  └── agent.chat_stream(augmented_msg) // SSE tokens → client

Phase 4 — Parallel Post-Processing:
  ├── memory_extraction (background)   // tokio::spawn, concurrent LLM call
  ├── answer_review (if enabled)       // concurrent on HTTP providers
  └── persist assistant response       // DB write
```

Key design decisions:
- **LLM classification on a concurrent thread**: `GiapToolAgent` uses the live LLM provider for reliable classification. The LLM call runs concurrently with I/O prep (message persist, memory prefetch) via `tokio::join!` in routes.rs — so the classifier overlaps with other work instead of blocking serially. On HTTP providers (Ollama/llamafile), the classifier runs on a separate HTTP connection, truly parallel.
- **Keyword routing** (`route_to_tool()` in `request_classifier.rs`) is available as a utility for fast pre-filtering but is not used in the primary path — LLM classification is more reliable for natural language.
- **InferencePool** (`pond-core/ports/inference_pool.rs`) provides provider-aware concurrency: 3 concurrent requests for HTTP providers (Ollama/llamafile), 1 for local GGUF (serialized by Goose's model mutex).
- **Memory injection is ON by default** (`agent_memory_inject: true`). Recent memories are injected into the system prompt every turn.
- **AnswerReviewer** uses the live provider (`RwLock`) so it always uses the currently loaded model.

See `docs/architecture/agent_pipeline.md` and `.ai/multi-threaded-agent-loop.md` for full details.

### Model Capabilities

`ModelCapabilities` struct (thinking, vision, audio, context_window, structured_output) is populated by each adapter from the model name. Used to drive: thinking mode in system prompts, context window budgeting, vision/image upload, capability badges in the UI.

See `docs/architecture/model_capabilities.md` for details.

### LLM Provider

Provider switch at runtime: `PUT /api/v1/settings` with `chat_provider` / `chat_model` triggers `rebuild_llm_provider()` in `pond-api/src/routes.rs`, which hot-swaps `AppState.llm_provider` (a `RwLock`). Supported providers: `"llamafile"` · `"ollama"` · `"local"` (in-process GGUF via llama-cpp-2). Platform-aware settings (Metal GPU offload, flash attention, context size) are applied automatically.

### GGUF Provider (`local`)

`LocalInferenceLlmAdapter` in `crates/pond-adapters-local-inference/src/lib.rs` wraps Goose's `LocalInferenceProvider`. Two construction paths:
- **HF format** (`"bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"`): `new_with_data_dir()` derives filename, registers in Goose's global `LocalModelRegistry`, then calls `new()`.
- **Raw filename** (`"gemma-4-E2B-it-Q4_K_M.gguf"`): `new_with_data_dir()` strips the `.gguf` extension as a synthetic registry ID, registers with `local_path = $data_dir/models/gguf/{filename}`, then calls `new(stem)`.

The registry is a global `OnceLock<Mutex<LocalModelRegistry>>` in Goose. Registration must happen before `LocalInferenceProvider::from_env()` is called, because it calls `resolve_model_path(model_id)` which looks up the registry by ID at first inference.

### SSE Streaming

Chat responses stream over Server-Sent Events at `POST /api/v1/chat/stream`. Each event is a JSON line:
- `{"type":"text","content":"...","token":"..."}` — partial token
- `{"type":"thinking","content":"..."}` — reasoning content (when `show_thinking` enabled)
- `{"done":true,"session_id":"...","model_role":"chat|think|task","usage":{"prompt_tokens":N,"completion_tokens":N}}`
- `{"error":"..."}` — provider error

Token usage comes from Ollama (`prompt_eval_count`/`eval_count`); llamafile only emits usage in its final SSE chunk (many builds omit it entirely); GGUF (via Goose) discards usage internally.

### Reasoning-Tag Stripping (ThoughtFilter)

Local models emit reasoning markup that must be stripped before reaching the user. Three tag formats exist:
- **Gemma 4**: `<|channel>thought…<channel|>`
- **Qwen3 / DeepSeek-R1**: `<think>…</think>`
- **Alternate**: `<thought>…</thought>`

The **authoritative filter** is `ThoughtFilter` in `crates/pond-api/src/thought_filter.rs` — a stateful, cross-chunk filter applied at the SSE layer (`routes.rs`). It handles all tag formats plus standalone sentinels (`<eos>`, `<|eos|>`, `<end_of_turn>`). **Do not add per-chunk tag stripping in adapters** — non-stateful stripping on streaming deltas interferes with ThoughtFilter's cross-chunk state and causes answers to be swallowed.

Tag stripping layers (defense-in-depth):
1. **Backend SSE** (`routes.rs`): `ThoughtFilter` — primary, handles all formats
2. **Non-streaming complete()** (`pond-adapters-local-inference/lib.rs`): `strip_thinking_tokens()` — for direct `LlmProvider::complete()` calls (reviewer, classifier)
3. **Desktop voice** (`tts_text.rs`): `filter_thinking()` — safety net for TTS pipeline
4. **Desktop display** (`canvas_feed.rs`): wraps `filter_thinking()` with persistent state
5. **Frontend** (`thinkFilter.ts`): JavaScript safety net for chat UI
6. **Reviewer** (`main.rs`): `strip_thinking_tags()` applied to revision responses

### Databases

Two SQLite databases in `$DATA_DIR` (macOS default: `~/Library/Application Support/goose-in-a-pond/`):
- `pond_system.db` — settings, sessions, devices, onboarding, profiles, memory, skills, MCP server configs
- `pond_logs.db` — sensor readings, camera events, telemetry

Migrations live in `crates/pond-infra/migrations/system/` and `migrations/logs/`, applied automatically via `sqlx::migrate!()` on startup. Cross-compile requires `SQLX_OFFLINE=true`.

### Desktop App (`pond-desktop`)

Tauri 2.0 app with three modes: **GUI sidebar** (React sections) · **Voice mode** (mic orb + TranscriptFeed) · **Canvas mode** (floating overlay, `Cmd+Shift+G`).

State lives in `src/state/` — a React context + `useReducer` pattern. All API calls go through `src/api/PondApiClient.ts` (singleton `api` export). In Tauri context, `AppContext.tsx` listens to Tauri events (`server-status`, `voice-state`) and auto-completes onboarding. In browser/Playwright context, it marks the server online immediately.

Playwright E2E tests in `tests/e2e/` use `helpers/api-mocks.ts` (`mockAllApiRoutes`) to intercept all API calls — no running pond-server required. Live E2E tests are gated by `GIAP_SERVER_URL` env var.

### Voice Pipeline

Whisper ASR (HTTP server at port 9000) → wake word detection → record → `POST /api/v1/transcribe` → chat. TTS via Piper subprocess or Piper HTTP server. Working voice config: `en_US-lessac-medium.onnx` (Piper).

### Scheduling System

Cron-based automation engine in `pond-infra-scheduler`. Tasks fire at cron intervals and execute prompts against the LLM agent or POST webhooks.

- **Domain**: `TaskKind` (AgentPrompt | Webhook), `Schedule`, `ScheduleRun`, `ScheduleResultEvent` in `pond-core/src/domain/schedule.rs`
- **Port**: `SchedulerPort` in `pond-core/src/ports/scheduler.rs` — create, list, delete, pause, resume, run_now, get_runs, list_upcoming
- **Executor**: `ScheduleExecutor` port → `AgentScheduleExecutor` creates ephemeral sessions (prefix `sched-`) and calls `agent.chat()`. `DeferredExecutor` breaks the circular init dependency (scheduler → agent → scheduler).
- **Adapter**: `CronSchedulerAdapter` in `pond-infra-scheduler/src/cron_scheduler.rs` — tokio-cron-scheduler, JSON persistence, `JsonRunHistory` for execution logs
- **MCP Tools (schedule)**: `list_schedules`, `create_schedule`, `delete_schedule`, `pause_schedule`, `resume_schedule`, `run_schedule_now`, `get_schedule_runs`
- **Natural language**: Tool classifier routes "schedule to get weather at 10am" → `create_schedule` → keyword parser extracts cron + prompt
- **Result delivery**: `ScheduleResultEvent` broadcast via `tokio::sync::broadcast` → SSE at `GET /api/v1/schedules/events` → desktop notification
- **Settings**: `schedule_result_notify` (default true)

### MCP Extension System & Marketplace

Extensions add tools to the agent at runtime via the Model Context Protocol. GIAP manages them through a layered architecture:

- **Port**: `ExtensionManagerPort` in `pond-core/src/ports/extension_manager.rs` — list, add, remove, toggle extensions
- **Adapter**: `GiapGooseExtensionManager` in `pond-adapters-goose/src/extension_manager.rs` — wraps Goose's native MCP client. Tracks extension status and errors in memory, validates commands before connecting.
- **Persistence**: `McpServerRepository` in `pond-infra/src/sqlite_mcp_servers.rs` — configs stored in `mcp_servers` table (name, kind, command, args, env, uri, enabled). Enabled extensions auto-reconnect on server restart.
- **Tool Registry**: `ToolRegistryPort` in `pond-core/src/ports/tool_registry.rs` — unified view of built-in + extension tools. Synced automatically when extensions are added/removed/toggled. Wired into `AppState.tool_registry`.
- **Marketplace**: `ExtensionMarketplace` port → `BundledMarketplace` service parses `crates/pond-core/src/extensions/marketplace_registry.json` (embedded at compile time via `include_str!`). 8 curated extensions: Filesystem, GitHub, SQLite, Brave Search, Memory, Puppeteer, Fetch, Slack.

Extension types: **stdio** (subprocess, stdin/stdout) · **streamable_http** (remote HTTP) · **builtin** (GIAP's native tools).

API endpoints:
- `GET /api/v1/extensions` — lists live + persisted-but-disabled extensions (merged view)
- `POST /api/v1/extensions` — register new extension (validates command/URI first)
- `DELETE /api/v1/extensions/{name}` — remove and delete persisted config
- `PATCH /api/v1/extensions/{name}` — toggle enabled/disabled (persisted to SQLite)
- `GET /api/v1/marketplace` — curated extensions from bundled registry
- `POST /api/v1/marketplace/{id}/install` — one-click install with persistence and tool registry sync

`ExtensionInfo` includes `status` ("connected" | "error" | "loading" | "disabled") and `last_error` for UI feedback. Desktop UI has "Installed" and "Browse" tabs.

Extension templates for building custom MCP servers: `templates/extensions/python/`, `templates/extensions/typescript/`, `templates/extensions/rust/`. Full developer guide at `docs/developer/extensions.md`.

MCP protocol compliance, version tracking, and authorization model documented in `docs/architecture/mcp-compliance.md`.

### Built-in MCP Tools

All tools defined in `crates/pond-mcp-server/src/giap_server.rs` using the `rmcp` `#[tool]` macro. Tool definitions for the system prompt in `crates/pond-core/src/prompts.rs` (`giap_tool_definitions()`).

- **Schedule** (7): `list_schedules`, `create_schedule`, `delete_schedule`, `pause_schedule`, `resume_schedule`, `run_schedule_now`, `get_schedule_runs`
- **Memory** (3): `save_memory`, `recall_memories`, `forget_memory`
- **Knowledge** (2): `search_wikipedia`, `wikipedia_get_article`
- **System** (6): `get_current_time`, `get_system_info` (via `sysinfo` crate), `send_notification` (via `notify-rust`), `run_shell_command` (sandboxed allow-list), `read_file`, `write_file`
- **Other** (4): `get_current_weather`, `list_registered_devices`, `get_current_profile`, `get_model_config`

`run_shell_command` only allows: ls, cat, echo, date, uptime, df, free, whoami, hostname, pwd, wc, head, tail, sort, uniq, grep, find, which, env, printenv. 10-second timeout.

`read_file`/`write_file` reject path traversal (`..`) and require absolute paths.

### Memory System (Enhanced)

Segment-based memory with importance scoring, decay, and automatic extraction from conversations. Inspired by boop-agent.

- **Segments**: `MemorySegment` — Identity (0.8), Correction (0.9), Preference (0.7), Relationship (0.7), Project (0.6), Knowledge (0.5), Context (0.3). Each has default importance and tier.
- **Tiers**: `MemoryTier` — Short (decay 0.1), Long (decay 0.01), Permanent (no decay). Identity defaults to Permanent, Context to Short.
- **Lifecycle**: Active → Archived → Merged. Archived memories are hidden from recall. Merged memories link via `superseded_by`.
- **Background extraction**: `MemoryExtractor` port → `LlmMemoryExtractor` runs after each chat turn (tokio::spawn, never blocks SSE). Compact prompt (~150 tokens) for 3B models. Max 3 facts per turn. Rate-limited to 10s intervals.
- **Decay formula**: `effective_score = importance * exp(-decay_rate * days) * (1 + ln(access_count + 1) * 0.1)`. Below 0.05 → prune, below 0.15 → archive. Cleanup runs every 6 hours.
- **Consolidation**: `MemoryConsolidator` port → single-pass LLM merge/prune (simplified from boop's 3-phase). Runs every 24 hours. Max 20 memories per batch.
- **Auto-classify**: `auto_classify_segment()` uses keyword heuristics (no LLM) — "I prefer" → Preference, "My name is" → Identity, etc.
- **MCP Tools**: `save_memory` (accepts segment/importance/tier), `recall_memories` (returns segment metadata, records access), `forget_memory` (delete by ID or content)
- **Settings**: `memory_extraction_enabled` (default **true**), `memory_cleanup_enabled` (default **true**), `memory_consolidation_enabled` (default false), `agent_memory_inject` (default **true**), `agent_memory_limit`

### Token Usage Tracking

Per-session token accumulation with estimated usage from the agent stream.

- **Estimation**: GooseAdapter tracks `system_prompt_len` + accumulated `full_text_len` during streaming. Emits `UsageStats` in `AgentStreamEvent::Done` using chars/4 heuristic (~estimated).
- **Storage**: `sessions` table has `total_prompt_tokens`, `total_completion_tokens`, `model_name` columns (migration 0016). Incremented after each chat response via `SessionStorage::increment_usage()`.
- **API**: `GET /api/v1/usage/summary` returns aggregate `{ total_prompt_tokens, total_completion_tokens, total_tokens, session_count }`. `GET /api/v1/sessions` includes per-session token counts.
- **Dashboard**: "Usage & Savings" card shows total tokens + money saved vs GPT-4o API ($2.50/M input + $10/M output).
- **Chat**: Session dropdown shows per-session token badges.

### Goose Submodule

`goose/` is a git submodule of Block's Goose agent framework. `pond-adapters-goose` and `pond-adapters-local-inference` depend on it. The outer GIAP workspace re-declares Goose's transitive dependencies (rmcp, sacp, tree-sitter-*) in the root `Cargo.toml` to resolve workspace version conflicts — do not remove them.

### Known model identifiers (dev/test)

- **Llamafile**: `gemma-2-2b-it.Q4_K_M` (served on port 8080 by default)
- **Ollama**: `gemma3:4b` · `gemma4:latest`
- **GGUF**: `gemma-4-E2B-it-Q4_K_M.gguf` (3.1 GB, at `$DATA_DIR/models/gguf/`)
- **Piper TTS voice**: `en_US-lessac-medium.onnx`

### Jetson Orin Nano Deployment

The primary deployment target is NVIDIA Jetson Orin Nano. The `Makefile` has cross-compilation targets:
- `make server` — CPU-only cross-compile via `cross` (requires Docker)
- `make deploy JETSON_HOST=user@ip` — cross-compile + scp to device
- CUDA builds must be done natively on the Jetson: `bash scripts/build-jetson-native.sh --cuda`
