# Component Breakdown

Each crate in `crates/` and the `pond-desktop/` app has a distinct responsibility within the hexagonal system. This document describes what each one does and how it relates to the others.

---

## Layer Overview

```
┌──────────────────────────────────────────────────────┐
│  Drivers (things that call the Core)                 │
│  pond-server (CLI + HTTP)  ·  pond-desktop (Tauri)   │
├──────────────────────────────────────────────────────┤
│  Application / API Layer                             │
│  pond-api  (Axum router + REST DTOs)                 │
├──────────────────────────────────────────────────────┤
│  DOMAIN CORE  (never imports framework deps)         │
│  pond-core: user_data/ · models/ · mcp/ · security/  │
│            · shared/  (each: domain/ ports/ services/)│
├──────────────────────────────────────────────────────┤
│  Driven Adapters (implement Core ports)              │
│  pond-infra · pond-infra-scheduler                   │
│  pond-adapters-goose · pond-adapters-weather         │
│  pond-adapters-whisper · pond-adapters-piper         │
│  pond-adapters-ollama · pond-adapters-llamafile      │
│  pond-adapters-local-inference · pond-adapters-mcp-memory │
│  pond-mcp-server                                     │
└──────────────────────────────────────────────────────┘
```

---

## Core Crates

### `pond-core` — The Brain

The heart of GIAP. Contains all domain logic with zero external framework dependencies.

Files are grouped into four quadrants around the user, plus a `shared/` module
for agent-loop plumbing. Every quadrant carries its own `domain/`, `ports/`, and
`services/` (and a `mocks/` for test doubles).

| Quadrant | Purpose |
|---|---|
| `src/user_data/` | Facts about / owned by the household: `profile`, `memory`, `session`, `settings`, `skill`, `recipe`, `prompt_template`/`prompt_extra`, `schedule`, `onboarding`, `sensor`, `draft`, `face` |
| `src/models/` | Anything that runs or routes inference: `message`, `model_record`, `model_capabilities`, providers, `inference`/`inference_pool`, `embedding`, `voice_input`/`voice_output`, `wake_word`, catalog/storage/downloader, plus context-budget / prompt-builder / history / thought-filter services |
| `src/mcp/` | The tool surface: `extension_manager`, `marketplace`, `mcp_server`, `mcp_knowledge`, `notification`, and `tools::{registry, dispatcher, caller, cache, agent}` |
| `src/security/` | The enclosing boundary: `secret`, `handshake`, `telemetry`, `event_log`, `policy`, `turn_metrics`, `oauth_provider` |
| `src/shared/` | Agent-loop plumbing used by every quadrant: `agent` types, `chat` (run_loop), `stdin_input`, `print_output` |

Within each quadrant: `domain/` holds pure Rust types, `ports/` holds the
`async_trait` interface definitions (one file per capability), and `services/`
holds the use-case orchestrators (`ChatService`, `ContextCompactor`, …).

**Key invariant:** `pond-core` must never import from `goose::*`, `sqlx::*`, `axum::*`, or any HTTP/filesystem library.

### `pond-api` — HTTP Layer

Axum 0.8 router, route handlers, and shared DTOs. Exports `AppState` (the dependency container) and `build_router()`.

- All REST routes versioned under `/api/v1/`
- Authentication middleware (`auth_middleware`) validates Bearer tokens via `Handshake::validate_token()`
- Rate limiting middleware (100 req / 60s per client)
- Onboarding guard blocks protected routes until setup is complete

### `pond-server` — Composition Root

The runnable binary. Wires all adapters into `AppState` and starts the Axum server.

- Parses CLI arguments via `clap`
- Initializes both SQLite databases (`pond_system.db`, `pond_logs.db`)
- Registers the GIAP builtin MCP extension with Goose
- Starts `pond-api` HTTP server + optional voice loop

---

## Adapter Crates

### `pond-infra` — Persistence

SQLite repositories for all domain entities, implemented with SQLx.

| Module | Port it implements |
|---|---|
| `sqlite_session_storage` | `SessionStorage` |
| `sqlite_device_registry` | `DeviceRegistry` |
| `sqlite_settings` | `SettingsRepository` |
| `sqlite_memory` | `MemoryRepository` |
| `sqlite_skill` | `UserSkillRepository` |
| `sqlite_recipe` | `AgentRecipeRepository` |
| `sqlite_prompt_template` | `PromptTemplateRepository` |
| `sqlite_prompt_extra` | `PromptExtraRepository` |
| `mock_handshake` | `Handshake` (in-memory, for dev/test) |

Two databases, initialized via `sqlx::migrate!()` on startup:
- `pond_system.db` — sessions, devices, settings, memory, skills, recipes, prompts
- `pond_logs.db` — event log, sensor readings, camera events

### `pond-infra-scheduler` — Cron Scheduling

`CronSchedulerAdapter` wraps `tokio-cron-scheduler`. Uses 6-field cron format with a leading seconds field (`"0 0 8 * * *"` = 08:00 daily). Dispatches tasks via `WebhookTaskExecutor`.

### `pond-adapters-goose` — Goose Agent (Primary Inference)

Wraps [Block's Goose](https://github.com/block/goose) as the primary LLM agent.

`GooseAdapter::chat()` on every turn:
1. Loads `Settings` from the DB
2. Fetches the active prompt template and renders it
3. Injects `PromptExtra` records and `UserSkill` content into the system prompt
4. Optionally injects recent memory fragments
5. Hot-swaps the Goose provider when the model config changes
6. Auto-loads the `"giap"` builtin MCP extension
7. Runs the full Goose agentic loop and returns aggregated text + tool metadata

### `pond-adapters-weather` — Weather

`OpenMeteoWeatherAdapter` implements `WeatherProvider` using the free [Open-Meteo](https://open-meteo.com) API with a 15-minute TTL cache. Location is configured via `Settings.lat` / `Settings.lon`. Exposed to the LLM exclusively through the `giap__get_current_weather` MCP tool.

### `pond-adapters-whisper` — Speech Recognition

`WhisperInput` posts audio to a running [whisper.cpp](https://github.com/ggerganov/whisper.cpp) HTTP server (`POST /inference`) — no C++ bindings required.

`WhisperKeywordDetector` listens for a configurable trigger word (default: `"goose"`) to implement wake-word detection.

### `pond-adapters-piper` — Text-to-Speech

`PiperOutput` spawns a [Piper](https://github.com/rhasspy/piper) subprocess, pipes text to stdin, and plays the raw audio output via `rodio`. Supports `en_US-lessac-medium` (default) and other ONNX voice models.

### `pond-adapters-ollama` — Ollama Provider

HTTP client for the Ollama `/api/chat` endpoint. Implements `LlmProvider` with support for `with_max_tokens()`, `with_temperature()`, and `with_system_prompt()`. Used for wiring into `ChatService` in tests and non-Goose contexts.

### `pond-adapters-llamafile` — Llamafile / OpenAI-compat Provider

HTTP client for `POST /v1/chat/completions` — compatible with llamafile, LM Studio, and any OpenAI-format endpoint. Also usable with `OllamaProvider` by setting `OLLAMA_HOST`.

### `pond-adapters-local-inference` — In-process GGUF

Loads GGUF model weights directly into the process via `llama-cpp-2`. No server process required. First build takes 5–15 min (compiles llama.cpp). Enable with `--features local-inference`. For CUDA on Jetson: add `--features pond-adapters-local-inference/cuda`.

### `pond-adapters-mcp-memory` — Flat-file MCP Memory

`GooseMcpMemoryAdapter` implements `McpMemoryPort` using a flat JSONL file. Enable with `--features mcp-memory`. Memory fragments are injected into every system prompt turn.

### `pond-mcp-server` — GIAP as a Goose Builtin MCP Extension

Exposes GIAP's smart home capabilities to the Goose agent as 9 MCP tools under the `giap__` prefix. Implements `rmcp`'s `#[tool_router]` macro.

| Tool | Description |
|---|---|
| `giap__get_current_weather` | Weather via Open-Meteo |
| `giap__list_registered_devices` | Device registry |
| `giap__list_schedules` | Cron tasks |
| `giap__get_user_profile` | Name, timezone, location |
| `giap__get_model_assignments` | LLM role assignments |
| `giap__recall_memories` | Search memory fragments |
| `giap__save_memory` | Persist a memory fragment |
| `giap__get_recipe` | Fetch a Goose recipe YAML |
| `giap__list_skills` | Active user skills |

---

## pond-desktop — Native Desktop App

A [Tauri 2.0](https://tauri.app) application providing a native UI for macOS and Linux (Windows: future).

| Directory | Purpose |
|---|---|
| `src-tauri/` | Rust backend — window management, hotkeys, system tray, audio recording, TTS playback, server lifecycle |
| `src/` | React 19 + TypeScript frontend |
| `src/styles/` | Jarida design tokens and base CSS (offline fonts via fontsource) |
| `src/state/` | `AppState` reducer + `AppContext` (all Tauri event listeners), and `chatRunStore` — the live chat turn (see below) |
| `src/api/` | `PondApiClient` — single class for all REST calls |
| `src/modes/` | `GuiMode` (sidebar app), `VoiceMode` (full-window orb) |
| `src/sections/` | 10 GUI sections: Dashboard, Chat, Devices, Schedules, Memory, Skills, Models, Prompts, Settings, Agent |
| `src/canvas/` | `CanvasOverlay` — frosted-glass floating window |
| `src/components/` | Shared: `Sidebar`, `VoiceOrb`, `TranscriptFeed`, `ContextCard`, `StartupScreen` |

**Three modes:**
- **GUI mode** — sidebar navigation app (1280×820 window)
- **Voice mode** — full-window voice orb with transcript feed
- **Canvas mode** — always-on-top translucent overlay for ambient display

The desktop app starts `pond-server` automatically via the `ensure_server_running` Tauri command, polls `server_health`, and displays a branded startup screen while the server comes online.

### The chat turn is owned by the module, not the view

`GuiMode` picks a section with a `switch`, not a router, so pressing anything in
the sidebar **unmounts the section that was showing**. A chat turn cannot live in
that component: leaving Chat mid-answer would throw away the transcript, the
queued follow-ups and the streaming bubble, while the stream itself kept running
and decoded its tokens into state updates on a dead component, which React drops
silently. The answer arrived, was persisted, and was invisible to whoever asked.

So the turn lives in `src/state/chatRunStore.ts` — a module singleton read through
`useSyncExternalStore`, the same shape `hub/state/hubDataStore.ts` uses for
anything that must outlive a view. It owns the transcript, the busy flag, the
queue and its draining, and the loop that folds the `/chat/stream` frames.

| Concern | Owner |
|---|---|
| Transcript, streaming bubble, busy, queue, active session id | `chatRunStore` |
| Composer draft, attachment tray, which screen the section is on | The section |
| `sessionId`, `sessionToken`, `serverOnline`, context cards | `AppContext` |

Two surfaces subscribe — `sections/Chat.tsx` and `hub/views/ChatHub.tsx` — and
they render **one** conversation rather than keeping one each; the Hub draws a
projection of the shared message, which is what stops the two drifting over what
a frame means. Being at module scope, the driver cannot read app state or
dispatch, so `AppContextProvider` installs a small typed bridge
(`setChatRunBridge`) carrying the auth token and the three callbacks the turn
needs on the way back. It is installed there for the same reason the schedule
SSE listener is: a turn started in Chat is still arriving while you are looking
at Devices.

Returning to Chat lands on the "All chats" wall as it always has, with one
carve-out: a turn still running, or one that finished while nothing was mounted
to show it, opens straight into its thread and is marked read once shown.

**Boundary.** This survives navigation, not a reload. Reloading or restarting the
app drops the HTTP body, and the server treats that as cancel-on-purpose (see
`goose_agent.rs`'s cancellation drop-guard), so the run dies mid-turn and the
assistant message is never written. Surviving that needs the run detached from
its connection server-side, which is separate work.
