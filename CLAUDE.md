# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Working agreement (Jerry)

- **Commit / PR sign-off:** never add a Claude/AI `Co-Authored-By` line or a "Generated with…" trailer. Commits and PRs are Jerry's own work — the only author is **Jerry Ochieng Anyumba**, written in the first person ("I"/"me"). This applies to commit messages, PR descriptions, and any authored prose.
- **No emojis in UI or code.** Use `lucide-react` icons for functional icons and the official logo for brand — never emojis (a lint guard, `no-emoji.test.ts`, scans source *including comments*).
- **Trust the model for tool use / thinking** — no keyword pre-classification, no separate ToolCaller model in the main loop. The main LLM calls MCP tools natively.

---

## What GIAP is

**Goose In A Pond (GIAP)** is a privacy-first, fully-local AI smart-home assistant. All inference, voice, and memory run 100% on-device — primarily targeting the **NVIDIA Jetson Orin Nano** — with no mandatory cloud dependency. It is built on [Block's Goose](https://github.com/aaif-goose/goose) agent framework and extends it with a smart-home layer: device registry, Whisper ASR + Piper TTS voice I/O, cron scheduling, a sensor/event-triggered rules engine, on-device vision event detection, SQLite-backed memory, a Tauri desktop app, and a mobile companion ("Goose On The Go" / GOTG) that drives the authenticated REST API.

## Architecture (Hexagonal — Ports & Adapters)

The domain core never imports Goose or any framework; it talks only through `async_trait` interfaces. The dependency direction is always **inward** toward `pond-core`.

```
pond-server   binary — parses CLI, wires every adapter into the app, serves HTTP
pond-api      Axum HTTP router + REST DTOs (+ embeds the web UI, see below)
pond-core     pure domain — NO external framework deps. Organised by bounded context,
              each with ports/ (trait interfaces) + services/ (logic) + domain types:
                models/      inference providers, embeddings  (InferenceProvider port)
                mcp/         tool registry, dispatcher, extension manager
                user_data/   devices, schedules, memory, settings, sessions, skills,
                             onboarding…
                security/    pairing / handshake / event log
                shared/      ChatService (turn persistence + memory extraction)
pond-infra              SQLite via SQLx — pond_system.db (authoritative) + pond_logs.db
pond-infra-scheduler    tokio-cron-scheduler adapter
pond-adapters-goose     wraps Block's Goose agent (GooseAdapter — the LIVE engine)
pond-adapters-*         ollama / llamafile / whisper / piper / weather / face-onnx /
                        vision (on-device camera event detection) /
                        local-inference (in-process GGUF via llama-cpp-2) / mcp-memory
pond-inference          independent in-process GGUF engine (llama.cpp, no Goose dep)
pond-agent              independent PondAgent loop — QUARANTINED (see Q2-05 below)
pond-mcp-server         exposes GIAP capabilities as a Goose builtin MCP extension
pond-desktop            Tauri 2 desktop app (React + TS front end; Rust shell)
```

Detailed design docs live under `docs/architecture/` and `docs/creating-ports-and-adapters.md`. Longer-form strategy / research notes are in `.ai/` (which is gitignored — durable notes belong in `docs/`).

---

## Commands

CI is the source of truth for canonical invocations: `.github/workflows/ci.yml`.

### Rust workspace
```bash
cargo fmt --check                                   # format gate (run `cargo fmt` to fix)
cargo clippy -p pond-core -p pond-api …             # lint the "fast crates" set (see ci.yml)
cargo test  -p pond-core -p pond-api …              # test the fast crates
cargo test  -p <crate> <test_name>                  # run ONE test (substring match)
cargo test  -p <crate> -- --ignored                 # the ~28 LIVE-HARDWARE tests (skipped by default)
cargo check -p pond-server -p pond-adapters-goose   # PRODUCTION-binary compile gate (pulls goose submodule; slow cold)
```

- The **"fast crates"** are every crate that does **not** pull the Goose submodule or heavy native libs (goose, llama-cpp-2, candle, onnxruntime). The exact list is in `ci.yml`; `pond-server`, `pond-adapters-goose`, `pond-adapters-local-inference`, `pond-adapters-face-onnx`, and `pond-agent` are excluded from the fast lint/test pass and covered only by the `cargo check` gate.
- `SQLX_OFFLINE=true` is required to build offline. Note: session/settings storage uses **runtime `sqlx::query`** (not the `query!` macro), so there is **no `.sqlx/` dir** and `cargo sqlx prepare` is not needed. Settings persist as a **flat key-value table** (`settings(key,value,updated_at)`), one row per field — new fields are new rows, no migration; the API serializes the `Settings` struct directly (no DTO). A completeness test (`every_settings_field_is_dispositioned`) fails the build if a new `Settings` field is not classified UI-wired or headless.
- CI overrides `RUSTFLAGS=""` — see the target-cpu landmine under *Goose Submodule*.

### Run the server
```bash
cargo run -p pond-server -- serve [--port PORT] [--open]   # dashboard; port order 80 → 8080 → 4000 → 5000
cargo run -p pond-server -- setup [--model tiny|base|small]
cargo run -p pond-server -- chat  [--provider mock|llamafile|ollama] [--model M]
cargo run -p pond-server -- status
```
Data (both SQLite DBs, logs, downloaded models) lives in the OS data dir, resolved by `default_data_dir()`. When run detached (no TTY, e.g. a systemd service), `serve` needs stdin kept open or it shuts down on stdin EOF — and on Linux enable `loginctl enable-linger` so the process survives SSH logout.

### Frontend (`pond-desktop/`)
```bash
npm ci
npm run dev            # Vite + auto-starts pond-server (concurrently)
npm run build          # Vite build -> pond-desktop/dist (embedded into the server binary)
npm test               # Vitest unit tests
npx playwright test    # E2E — self-mocked (page.route()); spins up its own Vite, no server needed
npm run tauri build    # native desktop app bundle
```
- E2E mocks use origin-agnostic `**/api/**` globs and pin the API base via `window.__GIAP_SERVER_URL__` in `tests/e2e/helpers/api-mocks.ts` (`mockAllApiRoutes`). `page.route` is **last-registered-wins** — register catch-alls before specific routes.
- In a plain browser the app defaults its API base to `window.location.origin` (so the single-executable dashboard works same-origin over the LAN); the Tauri shell injects `window.__GIAP_SERVER_URL__` for a local server. See `defaultServerUrl()` in `PondApiClient.ts`.

### Single-executable / Jetson build
`pond-server` embeds `pond-desktop/dist` at compile time (`crates/pond-api/build.rs` + `routes.rs` via `include_dir`), so a release build is a **single self-contained executable** (build the UI first, or you get the `build.rs` placeholder). Build scripts:
```bash
bash scripts/jetson.sh deploy                # from the dev machine: one-command deploy to the nano
bash scripts/jetson.sh build --cuda          # ON the Jetson: CUDA/GPU build (GPU build MUST be on-device)
bash scripts/jetson.sh docker-build          # OFF-device: aarch64 CPU binary via native linux/arm64 container
```
All Jetson scripts live in `scripts/jetson/` (see its README for the full workflow). Cross-build gotcha: ggml's cmake must not probe the build host's CPU. `scripts/jetson/ggml-toolchain.cmake` (via `CMAKE_TOOLCHAIN_FILE`) pins `GGML_NATIVE=OFF` + `GGML_CPU_ARM_ARCH=armv8.2-a+fp16+dotprod`. Use a valid `-march` ISA string for C/C++ (`-march=armv8.2-a+fp16+dotprod`) — a CPU *name* like `cortex-a78` is only valid for rustc's `target-cpu`, never gcc's `-march`.

### On-device inference (Jetson Orin Nano)
Decode is **memory-bandwidth-bound** (~102 GB/s on the 8GB Super): `tok/s ≈ 102 / model_GB`. A ~2GB 3B-Q4 model runs ~22 tok/s at 100% GPU; a 5.6GB model spills off-GPU → single-digit tok/s. Keep models ≤ the GPU budget (leave ~1GB headroom for KV cache). The Models tab + onboarding surface a fit-verdict from `GET /api/v1/models/memory-status` (note: the Ollama/llamafile NoopScheduler returns zeros → verdict "unknown"). See `docs/developer/inference_optimization.md` and `scripts/jetson/llama-optimization/` (drop_caches before `-ngl` to dodge the ~586 MiB NvMap OOM wall).

---

## Agent Pipeline

### Persistence ownership

`ChatService` (`crates/pond-core/src/shared/services/chat.rs`) is the **sole owner of turn persistence and memory extraction**.

Every chat turn — regardless of which engine path handles it — must go through these methods:

| Method | When to call |
|--------|-------------|
| `persist_user_message(&str)` | Before starting the agent stream |
| `persist_assistant_turn_with_extraction(tool_results, text, usage, model, user_msg)` | After the stream drains — persists the turn **and** spawns memory extraction |

When memory extraction is not needed (e.g. `agent_chat_stream`), use `persist_assistant_turn` instead. `persist_assistant_turn` also derives + persists a session **title** from the first user message (`ensure_session_title`, guarded on `title.is_none()`), so the chat-history sidebar always shows a readable topic, never a raw id. Note: the live Goose HTTP handlers build `ChatService` **without** an `LlmProvider`, so LLM-summarized titles do not run there — the deterministic first-message-derived title is the reliable path.

Both `/chat/stream` (`chat_stream` handler) and `/agent/chat/stream` (`agent_chat_stream` handler) build a `ChatService` and call these methods. The `chat_stream` handler additionally calls `.with_memory_extraction(extractor, service, repo)` on the builder so extraction is owned by the service, not the handler.

Do not add inline persistence or extraction blocks to handlers — they will be silently dropped in future refactors.

### Engine paths

| Route | Handler | Engine |
|-------|---------|--------|
| `POST /api/v1/chat/stream` | `chat_stream` | `GooseAdapter` (via `state.agent`) |
| `POST /api/v1/agent/chat/stream` | `agent_chat_stream` | `GooseAdapter` (via `state.agent`) |

Both paths share a single authoritative history store: `pond_system.db` (`session_messages` table), readable via `GET /api/v1/sessions/{id}/messages`.

### Single source of truth

All chat history is written to and read from `pond_system.db`. Goose maintains its own `sessions.db` internally but the GIAP REST API never reads from it — `pond_system.db` is authoritative for the UI and multi-device sync.

### MCP dispatch

`GooseAdapter` dispatches the 12 `giap-*` builtin MCP extensions via Goose's own extension manager (`giap_registration.rs`). `crates/pond-mcp-server/src/dispatcher.rs` (`McpToolDispatcher`, the `PREFIX_*` consts) is the **PondAgent** direct-dispatch path, which is quarantined (Q2-05) — so audit/vision tools that need deps installed by pond-server (`init_audit_deps`) are dispatched by Goose, not this dispatcher.

---

## Pond-Agent Quarantine (Q2-05)

`crates/pond-agent` is compiled but **not activatable at runtime**. Setting `agent_backend = "pond"` in Settings is rejected at two layers:

1. **API** — `PUT /api/v1/settings` with `agent_backend: "pond"` returns HTTP 422.
2. **Startup** — `serve()` in `main.rs` overrides `"pond"` → `"goose"` with a `tracing::warn!` before any backend is wired.

Do not remove either guard without a dedicated stabilisation milestone. The code is preserved so it compiles and can be enabled when the engine is production-ready.

> Consequence for design work: `pond-core`'s `InferenceProvider` port and `pond-inference`'s `LlamaCppEngine` feed the **quarantined** PondAgent loop, **not** the live serving path. The live path is `GooseAdapter`, which reaches inference/CUDA through Goose's own provider.

---

## Goose Submodule

See `docs/goose-patch-management.md` for the patch set carried on top of upstream (`aaif-goose/goose`) and the upstream-rebase procedure.

The submodule is pinned to `jarida-io/Goose:main` (upstream main synced
2026-07-25 + the GIAP patch set: ollama tool-less retry, extra featured Gemma 4
models; the old native_tool_calling/use_jinja patches are subsumed by upstream's
`ToolCallingMode`/`ChatTemplate`). Upstream declares `rmcp = "^1.4"`; the
workspace still forces `rmcp = "=1.5.0"`. CI clones the fork branch tip
directly, bypassing the stored submodule SHA — a BREAKING sync must be staged on
a side branch and fast-forwarded into fork `main` together with the parent-side
API port in one commit (parent `main`'s ci.yml still references
`giap-patches-rmcp-1.5` until this branch lands there). The goose crates'
`workspace = true` deps resolve against the PARENT `Cargo.toml` (mirror rules in
`docs/goose-patch-management.md`). After a sync lands, teammates must run:

```bash
git submodule update --init --recursive
```

Note: `.cargo/config.toml` sets `-C target-cpu=native` for on-device (Jetson)
performance; CI overrides it with an empty `RUSTFLAGS` because native-CPU
artifacts in a shared cache SIGILL across heterogeneous runners. The same landmine
applies to any cross/containerised build — override `target-cpu=native` explicitly
(e.g. `target-cpu=cortex-a78` for the Orin) or the binary may SIGILL on the target.
