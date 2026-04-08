# HOWTOAI.md — AI Agent Navigation Guide

This file is for AI agents (Claude Code, Gemini, Goose, etc.) working in this repository.
It documents *how* to navigate effectively — not what the code does (that's `CLAUDE.md`),
but how to move through it without wasting context, hitting known traps, or violating
architectural constraints.

**Core constraint**: Every AI capability must run fully offline on the target hardware.
Cloud APIs are optional fallbacks, never requirements. Before adding any AI feature, ask:
*can this run on a Jetson Orin Nano (8 GB) or a Raspberry Pi 5 (8 GB)?*

---

## 1. Orient Before Acting

**Always read these two files at the start of every session**, in this order:

```
.ai/scratchpad.md          — current progress, pending tasks, lessons learned
CLAUDE.md                  — architecture, commands, ports table, known bugs
```

`scratchpad.md` tells you *where the project is*. `CLAUDE.md` tells you *how it's structured*.
Without both, you will repeat decisions that have already been made — or unmade.

---

## 2. Build Strategy — Fast vs. Full

There are two compile paths with radically different costs:

| Command | Time | When to use |
|---|---|---|
| `cargo build -p pond-core -p pond-infra -p pond-api -p pond-server` | ~2 s | Default — any change that doesn't touch Goose |
| `cargo build --workspace` | ~10 min (first time) | Only when changing Goose-dependent crates |
| `cargo build -p pond-adapters-local-inference` | 5–15 min | Only when the GGUF inference path changes |

**Rule:** Never trigger a full workspace build to verify a core change. The fast targeted build skips Goose compilation entirely.

---

## 3. The Workspace Exclusion Constraint — Read This First

Several crates are intentionally **excluded from `[workspace.members]`** and listed in `[workspace.exclude]` instead:

- `pond-adapters-goose`
- `pond-mcp-server`
- `pond-adapters-mcp-memory`
- `pond-adapters-local-inference`
- `pond-adapters-weather`
- `pond-infra-scheduler`

**Why:** Goose's `code-mode` feature requires `rmcp ^0.14`; another feature requires `rmcp 1.2`. These are irreconcilable in a single workspace resolution.

**What this means for you:**
- Never add a Goose-dependent crate to `[workspace.members]` — the build will fail.
- Always use `default-features = false` on any `goose` dependency.
- Never enable the `code-mode` feature.
- Use `-p <crate>` directly to build/test these crates.

---

## 4. The Three-Layer Import Rule

```
pond-core  ←  pond-infra / pond-api / pond-adapters-*  ←  pond-server
```

- `pond-core` must never import from `goose::*`, `pond-infra`, or any adapter crate.
- If you see a Goose type leaking into `pond-core`, that is a bug — create a port instead.
- `pond-server` is the only crate that knows about all the others.

**How to verify:** `cargo build -p pond-core` should compile in under 1 second with zero Goose-related crates in the dependency graph.

---

## 5. How to Add a New Capability

Follow this exact sequence — skipping steps causes rework:

1. **Domain type** → `crates/pond-core/src/domain/<name>.rs` (pure Rust, no external deps)
2. **Port trait** → `crates/pond-core/src/ports/<name>.rs` (add `#[async_trait]` and the import manually — the code generator omits it)
3. **Mock** → `crates/pond-core/src/services/mock_<name>.rs` — write tests against the mock *before* building the real adapter
4. **Register the port** → `crates/pond-core/src/ports/mod.rs` (`pub mod <name>;`)
5. **Real adapter** → new crate or existing adapter crate (check if Goose already has it — see `CLAUDE.md` §Goose Built-in Providers)
6. **Wire** → `crates/pond-server/src/main.rs` via `Arc<dyn Port>`
7. **Add to `AppState`** if the REST API needs access → `crates/pond-api/src/lib.rs`

**Test first:** `cargo test -p pond-core` should pass before you touch any adapter.

---

## 6. Finding Things Quickly

| I need to find… | Look here |
|---|---|
| The current task list | `.ai/scratchpad.md` |
| Port trait definitions | `crates/pond-core/src/ports/` |
| Which adapter implements which port | `CLAUDE.md` §Existing Ports table |
| Where a port is wired up | `crates/pond-server/src/main.rs` |
| REST route definitions | `crates/pond-api/src/routes.rs` |
| SQLite schema / migrations | `crates/pond-infra/migrations/` |
| Goose API surface | `goose/crates/goose/src/` (the nested submodule) |
| All port constants (API, whisper, llamafile ports) | `crates/pond-server/src/ports.rs` |
| MCP tools exposed to the LLM | `crates/pond-mcp-server/src/giap_server.rs` |

---

## 7. Goose API — Known Gotchas

These cost significant debugging time and are documented so you don't repeat them:

- `PermissionManager::instance()` returns the manager directly — **do not wrap in `Arc`**.
- `Message` has no `.text()` — use `.as_concat_text()`.
- `Agent.provider` is private — use `agent.update_provider(provider, session_id)`.
- There is no `Role::System` in Goose — pass system content as the `system: &str` arg to `Provider::complete()`. `GooseProviderAdapter` already handles this.
- `GooseMessage.created` is a Unix timestamp (`i64`), not `DateTime<Utc>` — use `DateTime::from_timestamp(msg.created, 0)`.
- `SessionManager::instance()` uses a global singleton — use `GooseSessionAdapter::with_data_dir()` to isolate GIAP's storage.

---

## 8. SQLite — Known Gotchas

- `sqlx::query()` with multi-statement SQL silently executes only the first statement. **Always use `sqlx::migrate!()`** for schema changes.
- Migration files must exist on disk before `db.rs` is compiled (compile-time macro).
- `datetime('now')` in SQLite produces `"YYYY-MM-DD HH:MM:SS"` (no `T`, no timezone) — sqlx's chrono integration can't decode this. Parse manually: `NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")`.
- Add `rowid ASC` as a secondary sort on `datetime` columns — 1-second resolution needs a tiebreaker.
- Even if `chrono`, `uuid`, or `serde_json` are in `[workspace.dependencies]`, each crate must also list them in its own `[dependencies]`.

---

## 9. Port Management — Compile-Time Only

All network port constants live in **`crates/pond-server/src/ports.rs`**. There are no `--port` or `--whisper-url` CLI flags. If a port is blocked, the server tries the next port in arithmetic sequence automatically.

- GIAP-owned sockets: `bind_with_fallback(host, start)` — holds the socket, no TOCTOU.
- External child processes (whisper, llamafile, qwen-tts): `find_free_port(start)` — bind-test-release.

To change a port: edit `ports.rs` and recompile. Do not add runtime config for this.

---

## 10. Testing Discipline

```bash
# Fast iteration — run these constantly
cargo test -p pond-core
cargo test -p pond-infra
cargo test -p pond-api

# Slow — only when touching Goose-dependent code
cargo test -p pond-adapters-goose
```

Three tests in `onboarding_integration_test.rs` are **pre-existing failures** (not regressions from this branch): `chat_is_blocked_before_onboarding`, `devices_is_blocked_before_onboarding`, `settings_is_blocked_before_onboarding`. Ignore them.

---

## 11. Jetson CUDA Build — Native Only

Cross-compilation is **not possible** for the `local-inference` feature (nvcc requires the target arch). The workflow is:

```bash
# On x86 host — deploy the non-CUDA binary
./scripts/deploy-jetson.sh

# SSH into Jetson — build CUDA variant natively
./scripts/build-jetson-cuda.sh    # sets CMAKE_CUDA_ARCHITECTURES=87, CUDA 12.6
```

The `LocalInferenceLlmAdapter` automatically applies Jetson-optimised `ModelSettings` (`n_gpu_layers=99`, `flash_attention=true`, `use_mlock=false`, etc.) when compiled with `--features cuda`. These are patched into the Goose model registry at startup.

---

## 12. Weather, MCP, and the GIAP Extension

Weather is **not a domain port** — it is MCP-tool-only. The LLM calls `giap__get_current_weather` on-demand. Do not add weather to `AppState` or the system prompt.

The GIAP MCP extension pattern:
1. `GiapServiceHandles` (global `OnceLock`) holds `weather`, `device_registry`, `scheduler`
2. `register_giap_extension()` registers the factory with Goose's builtin extension registry
3. `GooseAdapter::new()` loads the tools when `ExtensionConfig::Builtin { name: "giap" }` is added

External MCP servers (those the user adds via REST) are **persisted to SQLite** (`mcp_servers` table) and reconnected automatically at startup via `SqliteMcpServerRepository`.

---

## 13. Hardware Constraints — Model Selection Rules

GIAP targets two primary edge platforms:

| Platform | RAM | GPU | Realistic model range | ASR | TTS |
|---|---|---|---|---|---|
| Jetson Orin Nano 8 GB | 8 GB unified | 1024-core Ampere, 40 TOPS | 1–7B Q4_K_M, 40–70 tok/s | whisper `base.en` | Piper medium |
| Raspberry Pi 5 8 GB | 8 GB | None | ≤3B Q4, ~10–20 tok/s | whisper `tiny.en` | Piper medium |
| Raspberry Pi 4 4 GB | 4 GB | None | HTTP provider only | whisper `tiny.en` | Piper small |

**Rules when choosing or adding models:**

1. **Quantize first.** Prefer GGUF Q4_K_M or Q5_K_M — halves RAM vs. full precision with minimal quality loss.
2. **Stay under 6 GB total model footprint** on Jetson. Reserve ~2 GB for OS, voice pipeline, and inference overhead.
3. **Embedding models** must be ≤150 MB (e.g. `nomic-embed-text`, `all-MiniLM-L6-v2`).
4. **ASR**: `tiny.en` (~39 MB) for Pi, `base.en` (~74 MB) for Jetson. Larger models are rarely justified for wake-word + command recognition.
5. **TTS**: Piper with `en_US-lessac-medium.onnx` (~60 MB) is the baseline. Do not add TTS that requires a running GPU server unless wrapped in `Option<>` in `AppState`.
6. **Never require a GPU for the core serve path.** GPU acceleration is additive (Jetson CUDA feature flag), not mandatory.

**The three-role LLM pipeline** (`ModelRole` in `pond-core/src/domain/model_role.rs`):

| Role | Purpose | Constrained-device guidance |
|---|---|---|
| `Chat` | Fast conversational replies | Jetson: 3–7B Q4. Pi: 1B or HTTP provider |
| `Think` | Analysis, multi-step reasoning | Jetson only: 7B Q4. Skip on Pi |
| `Task` | Tool use, scheduling, device control | Jetson: 3–7B with tool support. Pi: delegate to Chat |

`RequestClassifier` routes by keyword (zero overhead). All three roles fall back to `Chat` if not separately configured.

---

## 14. The Five AI Layers

GIAP is structured across five conceptual layers. When adding features, identify which layer owns the work:

```
┌────────────────────────────────────────────────────────┐
│  5. UX Layer — Voice pipeline, Web dashboard, GOTG     │
│               (pond-server, pond-api, web/, GOTG app)  │
├────────────────────────────────────────────────────────┤
│  4. Goose Layer — Agents, MCP tools, Memory, Recipes   │
│               (pond-adapters-goose, pond-mcp-server)   │
├────────────────────────────────────────────────────────┤
│  3. Shell Layer — Goose process, bash scripts, cron    │
│               (pond-infra-scheduler, scripts/)         │
├────────────────────────────────────────────────────────┤
│  2. GUI Layer — Web dashboard, emulators (optional)    │
│               (web/dist, tower-http ServeDir)          │
├────────────────────────────────────────────────────────┤
│  1. OS Layer — Linux, drivers, models on disk          │
│               (Cross.toml, deploy-jetson.sh, setup.sh) │
└────────────────────────────────────────────────────────┘
```

---

## 15. Planned MCP Ecosystem (Quarterly Roadmap)

These are the MCP servers planned in the project proposal. When implementing, each becomes a new crate under `crates/pond-mcp-*` following the `GiapMcpServer` pattern:

| MCP Server | Quarter | Purpose |
|---|---|---|
| `giap` builtin | **Live** | Weather, devices, schedules — already wired |
| Moonbeam MCP | Q2 | Android UI automation for SDK-less smart devices (Playwright-equivalent for Android) |
| Vision Event Detection MCP | Q2/Q3 | Local camera feeds — motion, pets, packages; no frames leave device |
| Offline ASR MCP | Q2 | Standalone voice/NLU server exposing transcription as an MCP tool |
| Sensor Aggregator MCP | Q2 | Temperature, humidity, motion over local protocols (MQTT, Zigbee, GPIO) |
| Routine / Scheduler MCP | Q2 | Natural-language routine creation backed by `CronSchedulerAdapter` |
| Privacy Audit MCP | Q3 | Query activity logs: "what did Goose do this hour?" |
| Inter-Agent Coordination MCP | Q3 | Multi-agent task delegation and shared context |

**Implementation pattern for a new MCP server:**
1. Add service handles to `GiapServiceHandles` (`pond-mcp-server/src/registry.rs`)
2. Implement `#[tool]` methods in a new `*_server.rs` alongside `giap_server.rs`
3. Register via `register_builtin_extension(name, spawn_fn)` in `main.rs`
4. Tool names auto-prefix as `<extension_name>__<tool_name>`

---

## 16. Memory and Self-Improvement Architecture

Current memory stack (in order of access speed):

| Layer | Implementation | Scope |
|---|---|---|
| In-context window | `trim_to_budget()` → 9,952 char cap | Current session |
| Context compaction | `ContextCompactor` → LLM summarization at 80% budget | Current session |
| Session persistence | `SqliteSessionStorage` → `pond_system.db` | Cross-restart |
| Semantic fragments | `SqliteMemoryRepository` → cosine similarity (stubbed) | Cross-session |
| MCP flat-file memory | `GooseMcpMemoryAdapter` (`--features mcp-memory`) | Cross-session |

**Q3 roadmap: self-improving prompts.** The `prompt_addendum` field in `Settings` is the hook point — the system will feed session logs through the Memory MCP and allow the LLM to rewrite its own addendum. Do not build prompt rewriting as a core feature; keep it behind the existing Settings API.

---

## 17. What GEMINI.md Got Wrong About This Repo

`GEMINI.md` describes the upstream `goose/` submodule — not GIAP itself. References to `crates/goose-mcp`, `crates/goose-server`, `ui/desktop`, `just generate-openapi`, and `.goosehints` describe the Goose project, not GIAP. Disregard those sections when working in GIAP's own code.

The accurate architecture is the hexagonal layout in `CLAUDE.md`. When `GEMINI.md` and `CLAUDE.md` conflict, `CLAUDE.md` is authoritative for this repository.
