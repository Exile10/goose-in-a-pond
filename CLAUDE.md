# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Working agreement (Jerry)

- **Commit / PR sign-off:** never add a Claude/AI `Co-Authored-By` line or a "Generated with…" trailer. Commits and PRs are Jerry's own work — the only author is **Jerry Ochieng Anyumba**, written in the first person ("I"/"me"). This applies to commit messages, PR descriptions, and any authored prose.
- **No emojis in UI or code.** Use `lucide-react` icons for functional icons and the official logo for brand — never emojis (a lint guard, `no-emoji.test.ts`, scans source *including comments*).
- **Trust the model for tool use / thinking** — no keyword pre-classification, no separate ToolCaller model in the main loop. The main LLM calls MCP tools natively.

### Personal Agentic Intelligence (P.A.I.) — mandatory recursive check

**Any work touching PAI-1 through PAI-8 starts by reading [`docs/architecture/pai/00-checklist.md`](docs/architecture/pai/00-checklist.md) and ends by updating it.** No exceptions, every session, however small the change.

The eight capabilities — proactivity, thinking, multi-agent orchestration, hard profile boundaries, large context, smart compaction, personal context streaming, privacy guardrails — are **equally weighted and mutually interdependent**. A change that satisfies one in isolation is not done. The checklist's section 2.2 is the interdependency test; run it against all eight, not just the one being worked on.

Two rules that cause the most damage when skipped:

- **Re-verify before you trust.** Every current-state claim in the PAI documents is stamped with the date it was verified. Line numbers rot. Grep for the symbol, not the `file:line` — and fix the document in the same change when a claim has gone stale.
- **Prerequisites must be LANDED, not merely DESIGNED.** [`docs/architecture/personal-agentic-intelligence.md`](docs/architecture/personal-agentic-intelligence.md) holds the dependency graph, the status ledger and the seven cross-cutting invariants. It decides what is eligible to be worked on.

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

**`bash scripts/giap.sh` is the primary interface** — a menu-driven front door for
install, build, service control, logs and diagnostics that auto-detects the host
(Jetson / Linux / macOS) and whether CUDA is usable. Non-interactive:
`giap.sh install|build|doctor|status|deploy|logs`, plus `--dry-run` and `-y`.
`giap.sh doctor` exits 1 on any FAIL and is the fastest way to find a broken
install. The raw commands below are what it runs, and remain the escape hatch.

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

- The **"fast crates"** are every crate that does **not** pull the Goose submodule or heavy native libs (goose, llama-cpp-2, candle, onnxruntime). The exact list is in `ci.yml`; `pond-server`, `pond-adapters-goose`, `pond-adapters-local-inference`, `pond-adapters-face-onnx`, `pond-agent`, and `pond-inference` are excluded from the fast lint/test pass and covered only by `cargo check` gates. The list is also the enforcement of the hexagonal invariant: if a crate in it grows a `goose` dependency, the split it names has quietly stopped existing. `pond-api` carried one for a single `Recipe::from_content` call, and `pond-inference` was in the list while building llama.cpp — both fixed 2026-08-06.
- `SQLX_OFFLINE=true` is required to build offline. Note: session/settings storage uses **runtime `sqlx::query`** (not the `query!` macro), so there is **no `.sqlx/` dir** and `cargo sqlx prepare` is not needed. Settings persist as a **flat key-value table** (`settings(key,value,updated_at)`), one row per field — new fields are new rows, no migration; the API serializes the `Settings` struct directly (no DTO). A completeness test (`every_settings_field_is_dispositioned`) fails the build if a new `Settings` field is not classified UI-wired or headless.
- CI overrides `RUSTFLAGS=""` — see the target-cpu landmine under *Goose Submodule*.
- **Clean up build artifacts when you finish a testing session.** This workspace builds the Goose
  submodule, llama.cpp, ONNX Runtime and candle, and `target/` reaches tens of gigabytes on a
  laptop that also holds the models. `target/debug/incremental` is the worst of it and the least
  valuable — it is a per-crate rebuild cache, not a dependency cache, so deleting it costs one
  recompile of the workspace crates and nothing else:

  ```bash
  rm -rf target/debug/incremental     # the usual culprit; recovers the most, costs the least
  cargo clean -p pond-api -p pond-core     # a specific crate's artifacts
  cargo clean                              # everything, including the slow submodule build
  ```

  Prefer the first. Reach for a full `cargo clean` only when you are done for the day, because
  rebuilding `pond-adapters-goose` and the submodule from cold is minutes, not seconds.

  **This is not housekeeping, it is a failure mode.** A run in the PAI programme died on
  `ld: write() failed, errno=28` with `target/debug/incremental` at 112 GB; deleting it recovered
  84 GB. A full disk presents as a LINKER fault, not as a disk fault, so it reads like a miscompile
  and gets debugged as one. Check `du -sh target` before diagnosing a strange link error.

### Run the server
```bash
cargo run -p pond-server -- serve [--port PORT] [--open]   # dashboard; default 4000, falls back 4000..4009
cargo run -p pond-server -- setup [--model tiny|base|small]
cargo run -p pond-server -- chat  [--provider mock|llamafile|ollama] [--model M]
cargo run -p pond-server -- status
```

The server writes the port it actually bound to `<data_dir>/.runtime_api_port` —
read that rather than assuming, since `--port` is optional and the fallback walks
`4000..4009` (`ports::API_SERVER` + `ports::MAX_TRIES`).
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

### Live testing — required before claiming anything works

**Green unit tests are not evidence that the pond starts.** Every test in the Rust
workspace runs against a database built by applying every migration to an empty file,
in one process, with the adapter under test constructed by hand. None of that
exercises startup ordering, migration application against a database that already has
rows, route registration, the auth middleware, or the wiring in `main.rs` — which is
where several real defects have been.

```bash
scripts/live-test.sh                 # build, start on a scratch data dir, assert, dig logs
scripts/live-test.sh --ui            # also build the web UI and drive it with Playwright
scripts/live-test.sh --no-build      # reuse the existing binary
scripts/live-test.sh --keep          # leave the server up to poke at by hand
```

Run it for **any change touching a migration, a route, a handler, or startup wiring**.
It does six things, and each exists because the alternative missed something real:

1. **Builds with `RUSTFLAGS=""`**, matching `ci.yml`. See the target-cpu landmine below.
2. **Starts against a scratch `POND_DATA_DIR`**, so a test run can never touch a real
   pond, and reads the port back from `.runtime_api_port` rather than assuming 4000.
3. **Asserts over real HTTP** (`scripts/live_checks.py`) — including the failure cases.
   A handler that compiles and a handler that returns the right status for a missing
   row are different claims.
4. **Restarts against the same directory**, which now has rows. A migration that only
   works on an empty database works exactly once, and every install after the first is
   an upgrade.
5. **Re-runs the auth checks on a second server with the loopback bypass OFF.** This is
   not fussiness: with `POND_DEV_ALLOW_LOOPBACK` set, every auth assertion passes
   regardless of what the allowlist does.
6. **Digs the logs**, for more than your own feature. `WARN` and `ERROR` lines that were
   already there are still findings.

**Two rules for writing live checks.**

- **Assert the status code before any body predicate.** A check written as
  `body.get("profile_id") is None` passes against an error payload, where every lookup
  returns `None` — so it reports the opposite of the truth. `expect()` in
  `live_checks.py` enforces the ordering; use it.
- **Ask whether production could ever produce your fixture.** A test whose *fixture* is
  unreachable tests a system that does not exist. `ProfileScope::Owner` was a no-op in
  production for a whole phase because every fixture that produced an owned row set
  `profile_id` by hand, which no code path did.

### Live UI testing (Playwright against a real server)

`npx playwright test` runs `tests/e2e/`, which mocks **every** API call with
`page.route()` against the Vite dev server. That is the right shape for component
behaviour and it **cannot catch an API contract change** — the mock keeps returning the
old shape long after the server stopped producing it.

`tests/e2e-live/` mocks nothing. It drives the dashboard the server actually serves,
talking to the server that actually built it:

```bash
cd pond-desktop && npm ci && npm run build          # or the server serves the placeholder
POND_LIVE_URL=http://127.0.0.1:4000 npx playwright test --config=playwright.live.config.ts
```

`scripts/live-test.sh --ui` does all of that in one command. The live config has no
`webServer` block on purpose — the server is owned by the script, which also does the
restart and no-bypass passes that Playwright should not be driving. `retries: 0`, also
on purpose: a live test that passes on the second attempt is telling you something
about startup ordering, and retrying hides it.

**The first thing it asserts is that the page is not the placeholder.** `build.rs` emits
a stub carrying `data-giap-placeholder` when `pond-desktop/dist` was never built, and a
binary shipping it looks like a working server until somebody opens a browser.

**Know what has no UI before writing a UI test for it.** PAI-1's identity work is
API-only: `pond-desktop/src` calls exactly one profile route, `GET /api/v1/profiles`.
There is no household-member removal, no session-identity binding, and no wake-on-face
control in the shipped app. A Playwright test of that feature would exercise nothing —
grep `pond-desktop/src` for the routes first, and if nothing calls them, say so in the
report instead of writing a test that passes vacuously.

### macOS specifics

Both scripts run on macOS and Linux. On a Mac:

- `libasound2-dev` is a Linux-only concern. If a build fails on `alsa-sys` there, that
  is the Linux path; on macOS `cpal` uses CoreAudio and needs nothing installed.
- `TMPDIR` is not `/tmp` — the script honours it, so scratch data lands in the real
  per-user temp dir. Anything hardcoding `/tmp` will silently diverge.
- Chromium for Playwright installs per-user via `npx playwright install chromium`;
  there is no system-wide `PLAYWRIGHT_BROWSERS_PATH` unless you set one.
- `pkill -f pond-server` matches your own shell's command line on macOS more eagerly
  than on Linux. The script tracks PIDs instead; do the same by hand.
- **Check nothing else is already on port 4000 before a live run**:
  `lsof -nP -iTCP:4000-4009 -sTCP:LISTEN`. A `serve --native` left running has no
  `POND_DATA_DIR`, so it is on the *real* data directory. `live-test.sh` now reads
  `.runtime_api_port`, fails hard when it is absent, and refuses to drive a listener
  whose pid it did not start — it previously assumed 4000 and wrote `user_name=LiveTest`
  and `chat_model=mock` into a real pond.

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

`GooseAdapter` dispatches the 17 `giap-*` builtin MCP extensions (64 tools when every toggle is on; `giap-draft` and `giap-toolkit` are always-on, `giap-device-control` rides the `ext_device_enabled` toggle, and **two** extensions ship with their toggle **off** — `giap-orchestrator` and, since PAI-8 P2, `giap-context`, the second for a prompt-budget reason as well as a consent one: two tool schemas in every turn cost real tokens on a 4 096-token window and can only answer "nothing found" until somebody connects a source) via Goose's own extension manager (`giap_registration.rs`). Count them with `grep -c 'register_builtin_extension(' giap_registration.rs`, which is the whole answer: **do not subtract anything**. The import is `use goose::builtin_extension::register_builtin_extension;` — no open paren, so the grep never counted it, and the old "minus the import" instruction here turned the right number into 14, which is the exact wrong number this programme has recorded twice. Two call sites pass a const rather than a string literal (`TOOLKIT_EXTENSION`, `ORCHESTRATOR_EXTENSION`), so grepping for `"giap-*"` string literals undercounts by two. `crates/pond-core/tests/registration_matches_the_catalog.rs` ties this sentence, the registration list and `tool_group.rs :: TOOL_GROUPS` to each other, and fails if any two disagree. `crates/pond-mcp-server/src/dispatcher.rs` (`McpToolDispatcher`, the `PREFIX_*` consts) is a **second, live** dispatch path — not a PondAgent-only one. It is bound into `AppState` unconditionally by `main.rs` and served by `POST /api/v1/tools/invoke` and `POST /api/v1/mcp/tools/call`, which run a tool with no chat turn and therefore no engine session in `_meta`. With no session there is no caller, and a policy check against an unknown caller is *permitted* under `PolicyMode::Audit`, not refused. So that path is deny-by-default: `routes.rs :: DIRECT_DISPATCH_ALLOWLIST` names the only tools reachable through it (device actuation for the Hub, read-only weather for MCP Apps). Adding a name there grants it to any paired client and to any sandboxed MCP App iframe — anything that decides, executes, or reads household memory belongs on the Goose path instead. Its registration list has also drifted from `giap_registration.rs`: audit, vision, sensors and toolkit are absent here, and audit/vision need deps installed by pond-server (`init_audit_deps`) that only the Goose path supplies.

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
2026-07-25 + the GIAP patch set, which is **five** patches as of 2026-08-07:
ollama tool-less retry, extra featured Gemma 4 models, llama.cpp `ProviderStats`
parity, thinking-only turns count as empty, llama.cpp prompt-session KV cache —
`docs/goose-patch-management.md` is the authoritative table, and this line said
"two" until PAI-6 P2 counted the rows. The old native_tool_calling/use_jinja
patches are subsumed by upstream's `ToolCallingMode`/`ChatTemplate`). Upstream declares `rmcp = "^1.4"`; the
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
