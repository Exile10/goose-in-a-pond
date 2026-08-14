# Goose In A Pond -- Q1 Progress Report

**Goose Grant Program -- Block**
**Reporting Period:** Q1 2026 (January -- March)
**Submitted:** May 2026
**Author:** Jerry Ochieng, Jarida

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Architecture Deep Dive](#2-architecture-deep-dive)
3. [Q1 Deliverables -- Detailed Status](#3-q1-deliverables--detailed-status)
4. [Technical Achievements Beyond Q1 Scope](#4-technical-achievements-beyond-q1-scope)
5. [Goose On The Go -- Early Architecture](#5-goose-on-the-go--early-architecture)
6. [Goose as a Harness -- Integration Architecture](#6-goose-as-a-harness--integration-architecture)
7. [Projected Q2 Features](#7-projected-q2-features)
8. [Metrics](#8-metrics)

---

## 1. Executive Summary

Goose In A Pond (GIAP) is a privacy-first, locally-deployed AI home assistant built on top of Block's open-source Goose agent framework. The system runs entirely on local hardware -- targeting the NVIDIA Jetson Orin Nano as its primary deployment platform -- using open-source language models, offline voice processing, and a modular extension architecture powered by the Model Context Protocol (MCP).

### Q1 Goal (from Proposal)

> "Successfully run Goose locally on Jetson hardware with voice input and offline LLM response for basic tasks like reminders, weather, or media commands."

### What Was Achieved

Q1 delivered the complete software foundation for that goal and substantially exceeded the original scope. The core platform -- 88,000+ lines of Rust across 16 crates, a Tauri 2.0 desktop application with 17,000+ lines of TypeScript/React, and a comprehensive REST API with 79 endpoints -- is fully functional on macOS and Linux x86_64. Cross-compilation infrastructure for the Jetson Orin Nano (ARM64) is built and validated; hardware testing awaits device availability.

Beyond the foundation, Q1 delivered several features originally scoped for later quarters: an extension marketplace with 7 curated MCP extensions, OAuth 2.0 PKCE authentication for third-party services, embedding-based semantic query routing, a desktop application with full extension/model/memory/schedule management, and 25 built-in MCP tools. These early deliveries de-risk Q2 and Q3 by establishing the integration patterns that smart home device control, mobile companion access, and self-improving agent behavior will build upon.

---

## 2. Architecture Deep Dive

### 2.1 Hexagonal Architecture (Ports and Adapters)

GIAP follows a strict hexagonal architecture. The central design rule: `pond-core` never imports Goose, SQLx, Axum, or any external framework. All I/O crosses a port trait boundary.

```
                         +----------------------------+
                         |       pond-server          |
                         | (binary: wires adapters    |
                         |  into AppState, starts     |
                         |  Axum + Tauri)             |
                         +---+--------------------+---+
                             |                    |
              +--------------+------+    +--------+-----------+
              |     pond-api        |    |   pond-desktop     |
              | (Axum HTTP router,  |    | (Tauri 2 + React   |
              |  SSE streaming,     |    |  19 + Vite)        |
              |  REST handlers)     |    +--------------------+
              +---------+-----------+
                        |
              +---------+-----------+
              |     pond-core       |
              | (pure Rust domain)  |
              |  domain/  ports/    |
              |  services/          |
              +---------+-----------+
                        |
         +--------------+--------------+
         |              |              |
  +------+-----+ +-----+------+ +-----+------+
  | pond-infra | | pond-mcp-  | | pond-      |
  | (SQLite,   | | server     | | adapters-* |
  |  keyring,  | | (25 tools) | | (one per   |
  |  embed)    | +------------+ |  external  |
  +------------+                |  service)  |
                                +------------+
```

**Why this matters.** The hexagonal boundary enforces a critical property: swapping the LLM provider from local GGUF to Ollama to Llamafile requires zero changes in domain logic. Adding a new capability -- say, a Zigbee device controller -- follows a repeatable 5-step pattern without touching existing code:

1. **Domain type** in `pond-core/src/domain/` -- pure Rust struct, no external imports
2. **Port trait** in `pond-core/src/ports/` -- `#[async_trait]` interface defining the contract
3. **Mock + tests** in `pond-core/src/services/` -- must pass before any real adapter exists
4. **Real adapter** as `crates/pond-adapters-<name>/` -- implements the port trait
5. **Wire** in `pond-server/src/main.rs` -- inject into `AppState`

This pattern has been exercised 10+ times during Q1 (weather, whisper, piper, ollama, llamafile, local-inference, goose, face-onnx, MCP memory, scheduler), proving it scales without accumulating coupling.

### 2.2 Crate Structure

GIAP is organized as a Cargo workspace with 16 crates, each with a single responsibility:

| Crate | Role | Key Dependencies |
|-------|------|------------------|
| `pond-core` | Pure domain logic: 26 domain types, 46 port traits, 40+ service modules. Zero external framework imports. | None (std only) |
| `pond-infra` | SQLite persistence via SQLx (two databases: system + logs), keyring secret storage, fastembed vector embeddings. 20 migration files. | sqlx, keyring, fastembed |
| `pond-api` | Axum HTTP router with 79 REST endpoints, SSE streaming for chat, OAuth callback handlers, rate limiting, onboarding gate middleware. | axum, tower, tokio |
| `pond-server` | Binary crate that wires all adapters into `AppState`, starts Axum server, handles CLI commands (serve, setup, chat). | All adapter crates |
| `pond-adapters-goose` | Wraps Block's Goose agent: `GooseAdapter` implements `Agent` port, `GiapGooseExtensionManager` manages MCP extension lifecycle. | goose (submodule) |
| `pond-adapters-ollama` | Ollama HTTP client implementing `LlmProvider` port. Streaming + non-streaming. | reqwest |
| `pond-adapters-llamafile` | Llamafile HTTP client implementing `LlmProvider` port. OpenAI-compatible API. | reqwest |
| `pond-adapters-local-inference` | In-process GGUF inference via llama.cpp (through Goose's `LocalInferenceProvider`). Metal GPU offload on macOS. | goose, llama-cpp-2 |
| `pond-adapters-whisper` | Whisper ASR HTTP client for speech-to-text transcription. | reqwest |
| `pond-adapters-piper` | Piper TTS subprocess/HTTP client for text-to-speech synthesis. | reqwest, tokio::process |
| `pond-adapters-weather` | Open-Meteo weather API client implementing weather tool. | reqwest |
| `pond-adapters-face-onnx` | ONNX-based face detection, embedding extraction, and anti-spoofing. SCRFD detector + ArcFace embeddings. | ort (ONNX Runtime) |
| `pond-adapters-gotg` | Goose On The Go companion device protocol adapter. | reqwest |
| `pond-adapters-mcp-memory` | MCP-based memory bridge for Goose's native Memory extension. | rmcp |
| `pond-mcp-server` | GIAP's own MCP server with 25 built-in tools across 6 categories. Runs in-process as a builtin extension. | rmcp |
| `pond-infra-scheduler` | Cron-based task scheduler using tokio-cron-scheduler. JSON persistence for execution history. | tokio-cron-scheduler |
| `pond-desktop` | Tauri 2.0 desktop application: React 19 + Vite frontend, Rust Tauri backend. 15 UI sections, 3 interaction modes. | tauri, react |

### 2.3 Agent Pipeline (Multi-Threaded)

Every chat message flows through a parallelized four-phase pipeline designed to minimize user-perceived latency:

```
User Message
     |
     v
+----+-------------------------------------------------------+
| Phase 1 -- Parallel Prep (no LLM, <50ms)                   |
|                                                             |
|   +------------------+  +------------------+  +----------+  |
|   | route_to_tool()  |  | persist user msg |  | LLM ping |  |
|   | (keyword routing)|  | (DB write, ~1ms) |  | (if GGUF)|  |
|   +------------------+  +------------------+  +----------+  |
+-------------------------------------------------------------+
     |
     v
+----+-------------------------------------------------------+
| Phase 2 -- Tool Execution (HTTP/DB, 100-500ms)              |
|                                                             |
|   Domain classifier routes query to tool domain             |
|   ToolAgent pre-fetches data (weather, wikipedia, DB)       |
|   Result injected as context for Phase 3                    |
+-------------------------------------------------------------+
     |
     v
+----+-------------------------------------------------------+
| Phase 3 -- Main Chat Stream (5-30s)                         |
|                                                             |
|   agent.chat_stream(augmented_message)                      |
|   SSE tokens streamed to client in real-time                |
|   ThoughtFilter strips reasoning tags cross-chunk           |
+-------------------------------------------------------------+
     |
     v
+----+-------------------------------------------------------+
| Phase 4 -- Parallel Post-Processing (background)            |
|                                                             |
|   +-------------------+  +-----------------+  +-----------+ |
|   | memory_extraction |  | answer_review   |  | persist   | |
|   | (tokio::spawn,    |  | (if enabled,    |  | response  | |
|   |  concurrent LLM)  |  |  concurrent)    |  | (DB)      | |
|   +-------------------+  +-----------------+  +-----------+ |
+-------------------------------------------------------------+
```

Key design decisions that differentiate this pipeline:

- **Domain-based tool dispatch with embedding classification.** Queries are classified into domains (weather, knowledge, scheduling, memory, system, media) using fastembed MiniLM-L6-v2 embeddings. Each domain has a tool filter that restricts which MCP tools are visible, reducing hallucinated tool calls.

- **Parallel post-processing.** Memory extraction, answer review, and response persistence all run concurrently via `tokio::spawn` after streaming begins. The user never waits for these operations.

- **InferencePool for provider-aware concurrency.** HTTP providers (Ollama, Llamafile) get 3 concurrent request slots. Local GGUF gets 1 (serialized by llama.cpp's model mutex). This allows the answer reviewer and memory extractor to run truly parallel on HTTP providers.

- **Context engineering.** The system prompt is assembled with primacy/recency placement: temporal context (date, time, timezone) at the top for KV-cache stability, tool descriptions in the middle, and domain-specific hints injected per query at the bottom as a dynamic suffix.

### 2.4 Reasoning-Tag Stripping (ThoughtFilter)

Local models emit reasoning markup (`<think>`, `<|channel>thought`, `<thought>`) that must be stripped before reaching the user. GIAP implements defense-in-depth with 6 stripping layers:

1. **Backend SSE** (`routes.rs`): `ThoughtFilter` -- the authoritative stateful, cross-chunk filter
2. **Non-streaming complete()** (`pond-adapters-local-inference`): `strip_thinking_tokens()` for classifier/reviewer
3. **Desktop voice** (`tts_text.rs`): `filter_thinking()` safety net for TTS
4. **Desktop display** (`canvas_feed.rs`): persistent state wrapper
5. **Frontend** (`thinkFilter.ts`): JavaScript safety net
6. **Reviewer** (`main.rs`): `strip_thinking_tags()` for revision responses

---

## 3. Q1 Deliverables -- Detailed Status

### 3.1 Prototype Running on Jetson Orin Nano

**Status: Build infrastructure complete. Awaiting hardware testing.**

The cross-compilation toolchain is fully operational:

- **Makefile targets** (`make server`, `make deploy`, `make desktop`): Cross-compile `pond-server` for `aarch64-unknown-linux-gnu` via the `cross` tool (Docker-based ARM64 compilation from macOS/x86).

- **`SQLX_OFFLINE=true`**: All SQLx queries have offline metadata checked in (`sqlx-data.json`), enabling cross-compilation without a live database on the build host.

- **CUDA support**: Native builds on the Jetson support CUDA-accelerated inference via `build-jetson-native.sh --cuda` and the `pond-adapters-local-inference/cuda` feature flag.

- **Unified install script**: `bash scripts/install.sh --jetson` handles submodule init, dependency installation, cross-compilation, database setup, and model downloads for Jetson deployment.

- **Deploy script**: `make deploy JETSON_HOST=user@ip` cross-compiles and SCPs the binary to the device in a single command.

- **Cross.toml**: Configures the Docker image for ARM64 cross-compilation with the correct linker, pkg-config sysroot, and OpenSSL paths.

What remains is physical hardware validation -- running the compiled binary on an actual Jetson Orin Nano and measuring inference latency, memory usage, and voice pipeline end-to-end timing.

### 3.2 Voice Agent with Wake Word and Basic Commands

**Status: Fully functional on macOS. CLI and desktop voice modes operational.**

The voice pipeline consists of three components:

**Whisper ASR (Speech-to-Text)**
- whisper.cpp server running at port 9000
- `pond-adapters-whisper` wraps the HTTP API (`POST /inference`)
- Model: `ggml-base.en.bin` (default), configurable via settings
- Endpoint: `POST /api/v1/transcribe` proxies audio to the local Whisper server

**Piper TTS (Text-to-Speech)**
- Piper subprocess or HTTP server
- Working voice: `en_US-lessac-medium.onnx` (validated and reliable)
- `pond-adapters-piper` handles subprocess management and audio synthesis
- Voice-optimized system prompt: instructs the model to avoid markdown, use short responses, and speak naturally

**Wake Word Detection**
- Configurable wake word (default: "goose")
- Detection pipeline in `pond-desktop/src-tauri/src/audio.rs`
- Desktop voice mode: microphone orb UI with waveform visualization
- CLI voice mode: `pond-server chat --input whisper`

**Two Voice Modes**
- **CLI mode**: Terminal-based voice interaction via `--input whisper` flag
- **Desktop mode**: Full GUI with VoiceOrb component, TranscriptFeed, and AudioWaves visualization. Wake word triggers recording, which is sent to Whisper for transcription, then to the agent for response, then to Piper for spoken output.

**Wake Word Calibration**
- Desktop UI for calibrating wake word sensitivity
- Documented in `docs/wake-word-calibration.md`

### 3.3 Local LLMs with Goose Memory and Prompts

**Status: Fully operational. Three providers hot-swappable at runtime.**

**LLM Providers**

Three providers are supported, all implementing the same `LlmProvider` port trait:

| Provider | Adapter Crate | Transport | Streaming | Notes |
|----------|--------------|-----------|-----------|-------|
| Local GGUF | `pond-adapters-local-inference` | In-process (llama.cpp) | Yes | Metal GPU offload on macOS. Wraps Goose's `LocalInferenceProvider`. |
| Ollama | `pond-adapters-ollama` | HTTP (port 11434) | Yes | Full model library. Supports `prompt_eval_count`/`eval_count` for token usage. |
| Llamafile | `pond-adapters-llamafile` | HTTP (port 8080) | Yes | OpenAI-compatible API. Single-model server. |

**Runtime Provider Hot-Swap**: `PUT /api/v1/settings` with `chat_provider` and `chat_model` triggers `rebuild_llm_provider()` in `routes.rs`, which atomically swaps the `AppState.llm_provider` behind an `RwLock`. No restart required.

**Validated Models**
- Gemma 4 E4B (7.5B params, Q4_K_M quantized, 4.6 GB) -- primary local model
- Gemma 3 4B via Ollama
- Gemma 2 2B IT via Llamafile

**Memory System**

Segment-based memory with importance scoring, exponential decay, and automatic extraction:

- **7 Segments**: Identity (0.8), Correction (0.9), Preference (0.7), Relationship (0.7), Project (0.6), Knowledge (0.5), Context (0.3). Each has a default importance weight.
- **3 Tiers**: Short (decay rate 0.1), Long (decay 0.01), Permanent (no decay). Identity defaults to Permanent; Context to Short.
- **Decay formula**: `effective_score = importance * exp(-decay_rate * days) * (1 + ln(access_count + 1) * 0.1)`. Below 0.05 triggers pruning; below 0.15 triggers archival.
- **Background extraction**: `MemoryExtractor` port with `LlmMemoryExtractor` adapter. Runs after each chat turn via `tokio::spawn` -- never blocks SSE streaming. Compact prompt (~150 tokens) optimized for 3B models. Rate-limited to 10-second intervals. Maximum 3 facts per turn.
- **Consolidation**: `MemoryConsolidator` runs every 24 hours. Single-pass LLM merge/prune of up to 20 memories per batch.
- **Lifecycle**: Active -> Archived -> Merged. Archived memories hidden from recall. Merged memories link via `superseded_by`.
- **Cleanup**: Runs every 6 hours. Prunes memories below threshold, archives declining ones.
- **MCP tools**: `save_memory` (accepts segment/importance/tier), `recall_memories` (returns segment metadata, records access for decay), `forget_memory` (delete by ID or content).

**Prompt System**

- **4 built-in templates**: Balanced, Concise, Technical, Warm
- **Jinja2-style rendering**: Variables `{{assistant_name}}`, `{{user_name}}`, `{{personality}}`, `{{timezone}}`, `{{location}}`, `{{prompt_addendum}}`
- **Database-backed**: Templates stored in SQLite, editable via REST API and desktop UI
- **Custom templates**: Users can create, edit, and activate custom templates
- **Prompt partitioning**: Static prefix (personality, rules) separated from dynamic suffix (tool results, temporal context) for KV-cache reuse on local inference
- **System prompt extras**: Per-key instruction blocks injected via `agent.extend_system_prompt()`, ordered by `sort_order`

### 3.4 Modular Architecture for Plug-in Smart Home Tools

**Status: MCP extension system fully operational. Marketplace with 7 curated extensions.**

**MCP Extension System**

Extensions are MCP servers that expose tools to the Goose agentic loop. GIAP supports two transport types:

- **stdio**: GIAP spawns the extension as a subprocess and communicates via stdin/stdout JSON-RPC. Primary transport for local extensions.
- **Streamable HTTP**: GIAP connects to the extension over HTTP. Used for remote or shared extensions.

Extension lifecycle is managed by `GiapGooseExtensionManager`:
- Add extensions via REST API (`POST /api/v1/extensions`), desktop UI, or marketplace
- Configurations persisted to SQLite (`mcp_servers` table) -- auto-reconnect on restart
- Enable/disable without removing config (`PATCH /api/v1/extensions/{name}`)
- Tools from all extensions unified in a single namespace, prefixed by extension name (e.g., `my-ext__my_tool`)

**Marketplace**

7 curated extensions available for one-click installation:

| Extension | Author | Category | Tools | Featured |
|-----------|--------|----------|-------|----------|
| Filesystem | Anthropic | Productivity | read_file, write_file, list_directory, search_files, get_file_info, list_allowed_directories | Yes |
| GitHub | Anthropic | Development | create_issue, list_issues, create_pull_request, search_repositories | Yes |
| Brave Search | Anthropic | Productivity | brave_web_search, brave_local_search | Yes |
| Memory (MCP) | Anthropic | Productivity | create_entities, create_relations, search_nodes, open_nodes | No |
| Puppeteer | Anthropic | Automation | navigate, screenshot, click, fill, evaluate | No |
| Slack | Anthropic | Communication | send_message, list_channels, read_channel, search_messages | No |
| Music (Spotify) | GIAP | Entertainment | play, status, control | Yes |

**Secret Management**

- `SecretRepository` port with keyring-backed adapter (OS-native credential storage)
- Extensions declare required secrets in the marketplace registry
- OAuth 2.0 PKCE flow for services like Spotify: bundled Client ID, local callback server, browser-based authorization, token storage in keyring
- Environment variable injection: secrets injected into extension subprocess environments, isolated per extension

**Extension SDK**

Starter templates for building custom MCP extensions in three languages:
- **Python** (`templates/extensions/python/`): Zero dependencies, raw MCP protocol over asyncio
- **TypeScript** (`templates/extensions/typescript/`): Node.js with tsx, readline-based
- **Rust** (`templates/extensions/rust/`): serde + serde_json only, synchronous I/O

Each template includes two example tools (`greet` and `timestamp`) and a README with setup instructions.

**Domain-Based Tool Routing**

Queries are routed to the appropriate tool domain using a two-stage classifier:

1. **Embedding classifier**: fastembed MiniLM-L6-v2 computes semantic similarity between the query and domain exemplars. Domains include Weather, Knowledge, Scheduling, Memory, System, Media, and General.
2. **Tool filter**: Each domain has a whitelist of relevant tools, preventing the model from seeing irrelevant tools and reducing hallucinated tool calls.

### 3.5 GitHub Repository with Code and Documentation

**Status: Complete. Repository is public with comprehensive documentation.**

**Documentation**

| Document | Path | Content |
|----------|------|---------|
| AGENTS.md | `/AGENTS.md` | Full architecture reference: crate structure, agent pipeline, voice pipeline, memory system, scheduling, databases, model capabilities, known identifiers |
| API Reference | `/docs/api.md` | 79 REST endpoints with request/response schemas, error codes, authentication |
| Extension Guide | `/docs/developer/extensions.md` | Step-by-step guide for building MCP extensions, protocol spec, testing, best practices |
| MCP Compliance | `/docs/architecture/mcp-compliance.md` | Protocol version tracking, transport support, primitive coverage, authorization model |
| Port/Adapter Guide | `/docs/creating-ports-and-adapters.md` | 5-step pattern with examples |
| Architecture Docs | `/docs/architecture/` | 12 documents covering pipeline, data flow, database schema, scheduling, memory, model capabilities, token tracking, visual workflow |
| Developer Docs | `/docs/developer/` | 12 documents covering clean code, debugging, inference optimization, voice pipeline, prompt system, model architecture |
| Install Script | `/scripts/install.sh` | One-command setup with options for desktop, Ollama, Llamafile, Jetson |

**Build and Deployment**

| Script | Purpose |
|--------|---------|
| `scripts/install.sh` | One-command project setup (submodule init, deps, build, DB, models) |
| `scripts/build.sh` | Production build with feature flags |
| `scripts/build-jetson-native.sh` | Native build on Jetson with optional CUDA |
| `scripts/build-jetson-cuda.sh` | CUDA-specific build for Jetson |
| `scripts/deploy-jetson.sh` | SCP deployment to Jetson |
| `scripts/dev.sh` | Development server with hot reload |
| `scripts/setup.sh` | Model and database setup |
| `Makefile` | Cross-compilation targets for Jetson |

### 3.6 System Prompt Template for Personalized Interaction

**Status: Fully operational with 4 built-in templates and customization.**

The prompt system is designed around personalization and extensibility:

- **4 built-in templates** (Balanced, Concise, Technical, Warm), each with a distinct personality and instruction set, seeded at first setup
- **Jinja2-style variable substitution**: `{{assistant_name}}`, `{{user_name}}`, `{{personality}}`, `{{timezone}}`, `{{location}}`, `{{prompt_addendum}}`
- **Context-first design**: Temporal context (current date, time, timezone) placed at the top of the prompt for primacy effect and KV-cache stability
- **Voice-mode prompting**: When voice mode is active, the system prompt instructs the model to avoid markdown, use natural spoken language, and keep responses brief
- **Domain-specific hints**: Injected per query based on the embedding classifier's domain classification (e.g., weather queries get location context; scheduling queries get timezone context)
- **System prompt extras**: Per-key instruction blocks that users or skills can inject, ordered by priority, enabling layered behavior customization
- **User skills**: Markdown instruction blocks stored in SQLite, injected as system prompt extras when active. Allow users to define custom agent behaviors (e.g., "When asked about lights, always check device registry first")
- **Web UI management**: Prompts section in the desktop app allows browsing, editing, and activating templates

---

## 4. Technical Achievements Beyond Q1 Scope

Several capabilities originally scoped for Q2-Q4 were delivered during Q1, strengthening the platform foundation:

### Extension Marketplace (Originally Q4)
The curated extension registry with one-click installation was originally planned for the community release quarter. Delivering it in Q1 means that Q2 smart home integrations can leverage the same install-and-configure pattern, reducing time-to-integration for each new device type.

### OAuth 2.0 PKCE Flow (Originally Q2-Q3)
Full OAuth 2.0 with Proof Key for Code Exchange, bundled Client IDs for frictionless sign-in, local callback server, and token storage in the OS keychain. The Music (Spotify) extension validates the flow end-to-end. This infrastructure is directly reusable for Q2 smart device integrations that require OAuth (e.g., Philips Hue, Samsung SmartThings).

### Token Auto-Refresh
Background worker refreshes OAuth tokens every 45 minutes before they expire, ensuring extensions with OAuth dependencies maintain continuous operation without user intervention.

### Embedding-Based Domain Classification
fastembed MiniLM-L6-v2 provides semantic query routing at sub-millisecond latency. This scales to Q2's device-specific intent routing (e.g., "turn off the living room lights" routes to the IoT domain and only surfaces device-control tools).

### Desktop Application with Full Management UI (Originally Q3-Q4)
The Tauri 2.0 desktop app delivers 15 UI sections covering every aspect of the system: Chat, Dashboard, Extensions, Models, Memory, Schedules, Settings, Prompts, Skills, Devices, Logs, Faces, Agent, and Canvas. Originally, the desktop app was planned as a late-stage polish item; delivering it early enables rapid iteration on the user experience.

### 25 Built-in MCP Tools
The GIAP MCP server exposes 25 tools across 6 categories:

| Category | Count | Tools |
|----------|-------|-------|
| Schedule | 7 | list_schedules, create_schedule, delete_schedule, pause_schedule, resume_schedule, run_schedule_now, get_schedule_runs |
| Memory | 3 | save_memory, recall_memories, forget_memory |
| Knowledge | 2 | search_wikipedia, get_wikipedia_article |
| System | 6 | get_current_time, get_system_info, send_notification, run_shell_command, read_file, write_file |
| Profile | 2 | get_user_profile, get_model_assignments |
| Other | 5 | get_current_weather, list_registered_devices, list_skills, get_recipe |

### Answer Reviewer
Post-inference quality evaluation system. When enabled, a concurrent LLM call evaluates the streamed response and can revise it. Uses the live provider via `RwLock` so it always uses the currently loaded model. Configurable pass threshold and maximum review rounds.

### Model Management
Full model lifecycle management across 5 categories (LLM/GGUF, ASR/Whisper, TTS/Piper, Face/ONNX, Embedding):
- Browse and search models from HuggingFace (GGUF) and GitHub (Llamafile)
- Download with progress tracking
- Activate/deactivate models per role (chat, think, task, ASR, TTS)
- Scan filesystem for manually-added models
- RAM budget estimation for local models
- Desktop UI: ModelPickerModal with category filtering and download status

### Face Recognition Pipeline (Experimental)
ONNX-based face detection, embedding extraction, and anti-spoofing:
- **SCRFD detector**: Real-time face detection with bounding boxes and landmarks
- **ArcFace embeddings**: 512-dimensional face embeddings for recognition
- **Anti-spoofing**: Liveness detection to prevent photo-based attacks
- Integrated as `pond-adapters-face-onnx` with port traits in `pond-core`

### Scheduling System
Cron-based automation engine for recurring tasks:
- 6-field cron expressions (seconds granularity)
- Two task types: AgentPrompt (fires an LLM query) and Webhook (POSTs to a URL)
- Schedule result notifications via SSE and desktop notifications
- Human-friendly schedule picker in the desktop UI (replaces raw cron input)
- Calendar view for visualizing upcoming scheduled tasks
- 7 MCP tools for natural-language schedule management ("schedule a weather check every morning at 8am")

---

## 5. Goose On The Go -- Early Architecture

The desktop application (pond-desktop) is architecturally designed as the foundation for the mobile companion app, Goose On The Go.

### Why Tauri 2.0

Tauri 2.0 was selected specifically because it supports mobile targets (Android and iOS) from the same codebase. The Tauri 2.0 mobile runtime uses the platform's native WebView (WKWebView on iOS, Android WebView), meaning the React frontend runs unmodified on mobile.

### API-First Design

All server communication flows through `PondApiClient.ts`, a singleton abstraction that encapsulates every REST endpoint:

```
+-------------------+     HTTP/REST      +----------------+
| PondApiClient.ts  | =================> | pond-api       |
| (frontend)        |                    | (79 endpoints) |
+-------------------+                    +----------------+
     |
     +-- getSettings()
     +-- sendMessage()
     +-- streamChat()
     +-- listExtensions()
     +-- installMarketplace()
     +-- downloadModel()
     +-- ...
```

This abstraction means the mobile app needs zero backend changes. The same `PondApiClient` connects to a GIAP instance over the local network, with the handshake authentication flow (`POST /api/v1/handshake`) already implemented for device registration.

### Three Interaction Modes

1. **GUI Sidebar**: 15 React sections for full management (Chat, Extensions, Models, Memory, etc.)
2. **Voice Mode**: Microphone orb with waveform visualization, TranscriptFeed, wake-word detection
3. **Canvas Mode**: Floating overlay activated via `Cmd+Shift+G`, designed for always-visible assistant access

### State Management

React context + `useReducer` pattern in `src/state/`:
- `AppContext.tsx` provides global state and dispatch
- `reducer.ts` handles state transitions with typed actions
- In Tauri context, listens to native events (`server-status`, `voice-state`) and syncs state
- In browser/Playwright context, marks server online immediately for testing

### The Path to Mobile

| Component | Desktop | Mobile (Projected) |
|-----------|---------|-------------------|
| Runtime | Tauri 2.0 (macOS, Linux, Windows) | Tauri 2.0 Mobile (Android, iOS) or React Native wrapper |
| Frontend | React 19 + Vite | Same React components in WebView |
| API Client | PondApiClient.ts over localhost | PondApiClient.ts over LAN/mDNS |
| Auth | Local bypass (loopback) | Handshake token via `POST /handshake` |
| Voice | Desktop audio APIs | Platform audio APIs |
| Notifications | Tauri notification plugin | Platform push notifications |

---

## 6. Goose as a Harness -- Integration Architecture

### Why Goose

Block's Goose was selected as the agent framework for GIAP because it provides:

1. **MCP-native tool calling**: Goose's agent loop natively discovers and calls MCP tools using structured schemas passed to the LLM's `tools` parameter. GIAP does not need to implement tool calling from scratch.
2. **Extension management**: Goose handles the lifecycle of MCP connections -- stdio subprocess spawning, HTTP transport, capability negotiation, tool registry.
3. **Active maintenance by Block**: As Block's flagship open-source agent project, Goose receives regular updates including new MCP protocol versions, provider support, and performance improvements.
4. **Session and conversation management**: Goose provides session persistence, message history, and multi-turn conversation handling.

### GooseAdapter -- The Integration Point

`GooseAdapter` in `crates/pond-adapters-goose/src/goose_agent.rs` (1,323 lines) implements GIAP's `Agent` port trait. This is where GIAP's hexagonal architecture meets Goose's agent framework:

```
+--------------------+
|     pond-core      |
|   Agent port trait |
|   (async_trait)    |
+--------+-----------+
         |
         | implements
         |
+--------+-----------+
|   GooseAdapter     |
|                    |
|   - wraps goose::  |
|     agents::Agent  |
|   - maps GIAP      |
|     sessions to    |
|     Goose sessions |
|   - overrides      |
|     system prompt  |
|   - manages MCP    |
|     extensions     |
+--------------------+
         |
         | delegates to
         |
+--------+-----------+
|   goose::agents::  |
|   Agent            |
|   (Block's Goose)  |
+--------------------+
```

### Session Management

GIAP maintains its own session IDs (UUIDs stored in SQLite). These are mapped to Goose's internal session IDs via `resolve_goose_session()`. This mapping is necessary because Goose uses its own sessions.db with foreign key constraints -- directly using GIAP's session IDs caused FK violations (a bug discovered and resolved during Q1).

### System Prompt Override

GIAP replaces Goose's default system prompt with a home-assistant personality while preserving Goose's tool calling mechanics. The prompt is assembled by `PromptBuilder` in `pond-core`:

```
[Static Prefix -- cached in KV]
  You are {{assistant_name}}, a private AI home assistant...
  Current time: 2026-04-09 08:00:00 UTC+3
  Rules: ...

[Tool Descriptions -- from MCP registry]
  Available tools: weather, memory, schedules, ...

[Domain Hints -- dynamic per query]
  The user is asking about weather. Location: Nairobi, Kenya.

[Memory Context -- if enabled]
  Known facts about this user: prefers Celsius, name is Jerry, ...

[Prompt Addendum -- user customization]
  Additional instructions...
```

Goose's agentic loop then appends its own tool-calling instructions and MCP tool schemas transparently.

### Extension Lifecycle

`GiapGooseExtensionManager` (280 lines) manages MCP extensions on Goose sessions:

1. **Add**: Validates config, connects to the MCP server, registers tools in the unified namespace
2. **Remove**: Disconnects, removes tools from the registry, deletes persisted config
3. **Enable/Disable**: Toggles visibility without dropping the connection
4. **Reconnect on restart**: Reads persisted configs from SQLite and re-establishes connections

### Goose Submodule

`goose/` is a git submodule pinned to a specific commit of Block's Goose repository. The outer GIAP workspace re-declares Goose's transitive dependencies (`rmcp`, `sacp`, `tree-sitter-*`) in the root `Cargo.toml` to resolve workspace version conflicts. This is necessary because Goose's `Cargo.toml` uses workspace-level dependency declarations that conflict with GIAP's own versions.

### MCP Protocol Compliance

| Component | Protocol Version | Notes |
|-----------|-----------------|-------|
| GIAP MCP Server | 2024-11-05 | Tools-only server; does not need 2025-03-26 features |
| Goose MCP Client | 2025-03-26 | Latest protocol version for connecting to external servers |
| rmcp crate | 1.2.0 | Pinned in both GIAP and Goose workspaces |

GIAP supports 3 of 4 MCP primitives: Tools (full), Resources (passthrough via Goose), Prompts (passthrough via Goose), Sampling (supported via Goose's ClientHandler). The GIAP server itself exposes only Tools, which covers all current use cases.

---

## 7. Projected Q2 Features

### From the Proposal

Q2's goal is: "Goose can control real-world smart home devices via native or sandboxed methods, both from pond and Goose On The Go."

Planned deliverables:
- **Goose On The Go mobile app MVP** -- remote control and push notifications
- **Android sandboxing / Moonbeam MCP** -- UI automation for SDK-less smart devices
- **Smart device integrations** -- lights, media centers, IR blasters, MQTT/Zigbee
- **Expanded voice** -- command chaining, contextual memory across turns
- **API and event trigger system** for local sensors (motion, temperature, door)

### Q1 Work That Feeds Into Q2

| Q1 Deliverable | Q2 Application |
|----------------|----------------|
| Extension marketplace | One-click install of device-specific MCP extensions (Zigbee, MQTT, Hue) |
| OAuth PKCE flow | Ready for smart device OAuth (Philips Hue, Samsung SmartThings, Ring) |
| Embedding classifier | Scales to device-specific intent routing ("turn off living room lights" -> IoT domain) |
| Music extension | Template for all media control integrations (pattern: OAuth sign-in, 3 tools, playback state) |
| PondApiClient.ts | Mobile app connects to same API -- zero backend changes needed |
| Handshake auth | Device registration flow ready for mobile clients |
| `install.sh --jetson` | Production deployment path for Jetson hardware |
| Sensor endpoints | `POST /sensors`, `GET /sensors/{device_id}` already implemented |
| Camera event endpoints | `POST /camera/events`, `GET /camera/events` already implemented |
| Device registry | `POST /devices`, `POST /devices/{id}/heartbeat` for IoT device tracking |
| Schedule system | "Turn on lights at sunset" -- cron-based automation with agent prompt execution |

---

## 8. Metrics

### Codebase

| Metric | Count |
|--------|-------|
| Rust source files | 233 |
| Lines of Rust code | 88,805 |
| TypeScript/React files (desktop) | 83 |
| Lines of TypeScript/React | 17,678 |
| Total lines of code (Rust + TS) | ~106,000 |
| Rust crates | 16 |
| Git commits | 341 |
| Files changed (total) | 608 |
| Insertions (total) | 171,191 |

### Architecture

| Metric | Count |
|--------|-------|
| Domain types (`pond-core/domain/`) | 26 |
| Port traits (`pond-core/ports/`) | 46 |
| Service modules (`pond-core/services/`) | 40+ |
| REST API endpoints | 79 |
| MCP tools (built-in) | 25 |
| MCP tools (Music extension) | 3 |
| Marketplace extensions | 7 |
| Database tables (system) | 21 |
| Database migrations (system + logs) | 20 |
| Prompt templates (built-in) | 4 |
| Extension SDK languages | 3 (Python, TypeScript, Rust) |

### Testing

| Metric | Count |
|--------|-------|
| Rust `#[test]` functions | 2,427 |
| Rust files with tests | 36 (pond-core alone) |
| Desktop unit test files (vitest) | 9 |
| Playwright E2E spec files | 7 |
| Playwright E2E test cases | ~61 |

### Supported Providers and Models

| Category | Count | Details |
|----------|-------|---------|
| LLM providers | 3 | Local GGUF (llama.cpp), Ollama, Llamafile |
| Model categories | 5 | LLM, ASR, TTS, Face, Embedding |
| Validated LLMs | 3 | Gemma 4 E4B (GGUF), Gemma 3 4B (Ollama), Gemma 2 2B (Llamafile) |
| ASR engines | 1 | Whisper (whisper.cpp) |
| TTS engines | 1 | Piper |
| Embedding model | 1 | MiniLM-L6-v2 (fastembed) |

### Desktop Application

| Metric | Count |
|--------|-------|
| UI sections | 15 |
| Interaction modes | 3 (GUI, Voice, Canvas) |
| React components | 16 |
| State management | React Context + useReducer |

### Documentation

| Metric | Count |
|--------|-------|
| Architecture docs | 12 |
| Developer docs | 12 |
| Total docs (including guides, READMEs) | 30+ |
| Build/deploy scripts | 8 |

---

## Conclusion

Q1 established GIAP as a complete, production-grade platform for privacy-first local AI assistance. The hexagonal architecture, MCP extension system, multi-provider LLM support, and desktop application provide a foundation that substantially de-risks the remaining quarters. The cross-compilation toolchain is ready for Jetson deployment; the extension marketplace and OAuth infrastructure are ready for smart device integrations; and the API-first design is ready for the mobile companion app.

The primary Q1 gap -- physical Jetson hardware validation -- is a deployment concern rather than a software one. All code paths are exercised on macOS and Linux x86_64, and the ARM64 cross-compilation is validated at the toolchain level. Hardware testing is the next immediate priority.

Q2 will focus on bridging the software foundation to physical devices: deploying to the Jetson, integrating real smart home hardware, and shipping the Goose On The Go mobile companion.
