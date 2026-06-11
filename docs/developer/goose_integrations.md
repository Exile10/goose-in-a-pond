# Goose and Pond Engine Integration

> **Last updated: May 18, 2026.** This document covers the dual-engine architecture where GIAP can run either the Goose agent (via submodule) or the independent PondAgent, selectable at runtime via the `agent_backend` setting.

## Dual-Engine Architecture

GIAP supports two inference backends, selectable at runtime via `PUT /api/v1/settings` with `agent_backend`:

| Setting | Engine | Crate | When to use |
|---------|--------|-------|-------------|
| `"goose"` | GooseAdapter | `crates/pond-adapters-goose/` | Production default, cloud fallback, complex multi-tool workflows |
| `"pond"` | PondAgent + LlamaCppEngine | `crates/pond-agent/` + `crates/pond-inference/` | Jetson/embedded, latency-critical, offline, minimal overhead |

### Why two engines?

The Goose submodule provides a mature agent framework with extension management, but:
- Its `InferenceRuntime` is a global singleton -- cannot coexist with GIAP's `LlamaCppEngine` in the same process
- It adds ~30s cold-start overhead from extension loading and session setup
- On Jetson, the KV-cache session persistence in PondAgent saves 5-15s per turn

PondAgent was built as a lightweight alternative that wraps `LlamaCppEngine` directly, giving GIAP full control over KV-cache management, tool dispatch, and prompt construction.

### Backend selection in main.rs

**File:** `crates/pond-server/src/main.rs:1558-1646`

```rust
// When "pond" is selected, skip Goose entirely to avoid the llama.cpp backend
// singleton conflict. PondAgent uses its own LlamaCppEngine directly.
let pond_agent_active = agent_backend == "pond";

if pond_agent_active {
    // Build PondAgent: LlamaCppEngine + McpToolDispatcher + PromptBuilder
    let eng = pond_inference::LlamaCppEngine::new(&data_dir)?;
    eng.load_model(&settings.chat_model, 99, true).await?;
    let dispatcher = pond_mcp_server::McpToolDispatcher::new(/* deps */);
    let agent = pond_agent::PondAgent::new(Arc::new(eng), tool_defs, /* deps */);
} else {
    // Build Goose backend via build_goose_backend()
    build_goose_backend(agent_backend, /* deps */).await
}
```

The two paths are mutually exclusive -- the llama.cpp backend singleton cannot be shared.

---

## PondAgent Architecture

**File:** `crates/pond-agent/src/agent.rs`

### Direct McpToolDispatcher Usage

PondAgent bypasses Goose's extension manager entirely. Instead, it holds a direct reference to `McpToolDispatcher` (from `crates/pond-mcp-server/src/dispatcher.rs`), which routes tool calls to GIAP's builtin MCP servers by name prefix.

Key difference from Goose:

| Aspect | Goose | PondAgent |
|--------|-------|-----------|
| Tool discovery | Extension manager scans MCP servers at session init | `dispatcher.tools_json()` queried LIVE each turn (agent.rs:391) |
| Tool JSON format | `format_tools()` serializes from internal state | `ToolDispatcher::tools_json()` produces identical OpenAI format |
| Schema caching | Cached at session creation | Fetched live, always current |
| Tool dispatch | Goose agentic loop manages lifecycle | `dispatcher.dispatch(name, args)` called directly (agent.rs:569) |

### Live tool schema fetching

On each turn, PondAgent queries the dispatcher for the current tool schemas rather than using a cached copy:

```rust
// agent.rs:391-411 — inside chat_stream()
let (tools, tools_json_override, compact_json_override) = if caps.tool_calling {
    if let Some(ref disp) = self.tool_dispatcher {
        let full_json = disp.tools_json().await;       // OpenAI-format JSON
        let compact_json = disp.compact_tools_json().await;
        let defs = disp.available_tool_definitions().await;
        // ...
    }
};
```

This means:
- Adding/removing MCP servers at runtime is immediately reflected
- Tool schemas always match the current server state
- No stale cache after MCP server configuration changes

### tools_json_override flow

The pre-formatted JSON from the dispatcher flows through `InferenceOptions` into the generation task:

```
ToolDispatcher::tools_json() → InferenceOptions.tools_json_override
  → generation_task() → apply_chat_template_oaicompat(tools_json)
```

This bypasses `tools_to_json()` re-serialization, ensuring the model sees the exact same schema format that Goose would produce.

---

## McpToolDispatcher

**File:** `crates/pond-mcp-server/src/dispatcher.rs`

Routes tool calls by name prefix to registered MCP servers:

```
"giap-weather__get_current_weather"
  → prefix: "giap-weather__"
  → bare name: "get_current_weather"
  → dispatched to: WeatherMcpServer
```

### Dynamic schema discovery

Tool schemas are queried from each `ServerHandler::list_tools()` at runtime, not hardcoded. At debug log level, the dispatcher logs the full JSON schema for every tool:

```rust
// dispatcher.rs:274
tracing::debug!(
    prefix = reg.prefix,
    tool = %name,
    description = %desc,
    schema = %serde_json::to_string(schema).unwrap_or_default(),
    "tool schema"
);
```

### Peer construction

rmcp requires a `Peer<RoleServer>` for `RequestContext`. The dispatcher creates one via a minimal `serve_directly()` on a `DuplexStream` -- the transport is a no-op, but provides a valid peer for calling `ServerHandler` methods.

---

## Goose Submodule

`goose/` is a git submodule (Block's Goose agent framework). GIAP re-declares Goose's transitive dependencies (rmcp, sacp, tree-sitter-*) in the root `Cargo.toml` -- do not remove them.

### rmcp Conflict Constraint

`pond-adapters-goose` is workspace-excluded because Goose's `code-mode` feature pulls in `pctx_code_execution_runtime` which requires rmcp ^0.14, while `pctx_config` requires rmcp 1.2. All Goose-dependent crates must:

- Use `default-features = false` on the `goose` dependency
- Never enable the `code-mode` feature
- Be added to the workspace `exclude` list

### Patched files

GIAP patches two files in the submodule:
- `goose/crates/goose/src/providers/local_inference.rs` -- OR-merge for `native_tool_calling`
- `goose/crates/goose/src/providers/local_model_registry.rs` -- same OR-merge in `enrich_with_featured_mmproj()`

Without these patches, non-featured model IDs (like `gemma-4-E2B-it-Q4_K_M`) get `native_tool_calling: false` overwritten on every inference call.

---

## Phase 1 — Pure Rust, No Goose Deps (workspace-safe)

These changes build with `cargo build`. Do this phase first.

### 1A. `SchedulerPort` trait — `pond-core`

**New file:** `crates/pond-core/src/user_data/ports/scheduler.rs`
**Modify:** `crates/pond-core/src/user_data/ports/mod.rs` — add `pub mod scheduler;`

```rust
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub label: String,
    /// Standard 5- or 6-field cron expression (e.g. "0 8 * * *")
    pub cron: String,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    pub paused: bool,
    pub currently_running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTaskRequest {
    pub id: String,
    pub label: String,
    pub cron: String,
    /// Arbitrary JSON payload delivered to the task executor on each fire.
    pub payload: serde_json::Value,
}

#[async_trait]
pub trait SchedulerPort: Send + Sync {
    async fn create_task(&self, req: CreateTaskRequest) -> Result<ScheduledTask>;
    async fn list_tasks(&self) -> Result<Vec<ScheduledTask>>;
    async fn delete_task(&self, id: &str) -> Result<()>;
    async fn pause_task(&self, id: &str) -> Result<()>;
    async fn resume_task(&self, id: &str) -> Result<()>;
    async fn run_now(&self, id: &str) -> Result<()>;
}
```

### 1B. `McpMemoryPort` trait — `pond-core`

**New file:** `crates/pond-core/src/mcp/ports/mcp_knowledge.rs`
**Modify:** `crates/pond-core/src/mcp/ports/mod.rs` — add `pub mod mcp_knowledge;`

```rust
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;

#[async_trait]
pub trait McpMemoryPort: Send + Sync {
    async fn remember(&self, category: &str, data: &str, tags: &[String], global: bool) -> Result<()>;
    async fn retrieve(&self, category: &str, global: bool) -> Result<HashMap<String, Vec<String>>>;
    async fn remove_category(&self, category: &str, global: bool) -> Result<()>;
    async fn remove_specific(&self, category: &str, content: &str, global: bool) -> Result<()>;
    /// Returns full context string to prepend to the system prompt.
    fn instructions(&self) -> String;
}
```

### 1C. `ContextCompactor` service — `pond-core`

**New file:** `crates/pond-core/src/models/services/context_compactor.rs`

Depends only on `LlmProvider` (already in pond-core). No external deps.

```rust
pub struct ContextCompactor {
    /// Fraction of context used before compaction triggers. Default: 0.80
    threshold: f64,
    /// Estimated max tokens for the model (4 chars/token heuristic).
    context_limit: usize,
}

impl ContextCompactor {
    pub fn needs_compaction(&self, messages: &[ChatMessage]) -> bool {
        let chars: usize = messages.iter().map(|m| m.content.len()).sum();
        (chars / 4) as f64 / self.context_limit as f64 >= self.threshold
    }

    /// Summarize the oldest 75% of messages, keep the newest 25% verbatim.
    /// Falls back to trim_to_budget on any LLM error.
    pub async fn compact(
        &self,
        provider: &dyn LlmProvider,
        messages: Vec<ChatMessage>,
    ) -> Result<Vec<ChatMessage>>
}
```

**Modify `ChatService`** (`crates/pond-core/src/shared/services/chat.rs`):
- Add `compactor: Option<ContextCompactor>` field
- Add `pub fn with_context_compactor(mut self, c: ContextCompactor) -> Self`
- In `chat_once`, call `compactor.compact()` before `provider.complete()`, with `trim_to_budget` as hard fallback

### 1D. `pond-infra-scheduler` crate (workspace-included)

**New crate:** `crates/pond-infra-scheduler/`

**Workspace `Cargo.toml` changes:**
```toml
# Add to [workspace] members:
"crates/pond-infra-scheduler",

# Add to [workspace.dependencies]:
pond-infra-scheduler = { path = "crates/pond-infra-scheduler" }
tokio-cron-scheduler = "0.14"
```

**`CronSchedulerAdapter`** implements `SchedulerPort`:
- Holds `TokioJobScheduler` + `Arc<Mutex<HashMap<String, (JobId, ScheduledTask)>>>`
- Persists task list to `$DATA_DIR/schedules.json`
- Job body: configurable `Arc<dyn TaskExecutor>`; initial impl is `WebhookTaskExecutor` (POSTs `payload` to `payload["webhook_url"]`)

**Tests:** create/list/delete/pause/resume, invalid cron returns error, persistence round-trip

### 1E. `AppState` additions — `pond-api`

**File:** `crates/pond-api/src/lib.rs`

```rust
pub mcp_memory: Option<Arc<dyn McpMemoryPort + Send + Sync>>,
pub scheduler: Option<Arc<dyn SchedulerPort>>,
```

Initialize both as `None` in all `AppState { .. }` construction sites.

### 1F. Schedule REST endpoints — `pond-api`

**File:** `crates/pond-api/src/routes.rs`

```
GET    /api/v1/schedules              list_schedules
POST   /api/v1/schedules              create_schedule
DELETE /api/v1/schedules/:id          delete_schedule
POST   /api/v1/schedules/:id/pause    pause_schedule
POST   /api/v1/schedules/:id/resume   resume_schedule
POST   /api/v1/schedules/:id/run-now  run_schedule_now
```

Return `503 SERVICE_UNAVAILABLE` when `state.scheduler` is `None`.

### 1G. Wire scheduler in `pond-server`

**File:** `crates/pond-server/Cargo.toml` — add `pond-infra-scheduler = { workspace = true }`

**File:** `crates/pond-server/src/main.rs`
```rust
let scheduler = CronSchedulerAdapter::new(data_dir.join("schedules.json"))
    .await
    .map(|s| Arc::new(s) as Arc<dyn SchedulerPort>)
    .ok();
```

**Verify Phase 1:**
```bash
cargo build
cargo test -p pond-core -p pond-infra-scheduler
curl http://localhost:4000/api/v1/schedules   # → []
```

---

## Phase 2 — MCP Memory (workspace-excluded)

### 2A. `pond-adapters-mcp-memory` crate

**Workspace `Cargo.toml` changes:**
```toml
# Add to exclude:
exclude = ["crates/pond-adapters-goose", "crates/pond-adapters-mcp-memory", ...]

# Add to [workspace.dependencies]:
pond-adapters-mcp-memory = { path = "crates/pond-adapters-mcp-memory" }
```

**`crates/pond-adapters-mcp-memory/Cargo.toml`:**
```toml
[dependencies]
pond-core = { workspace = true }
goose-mcp = { path = "../../goose/crates/goose-mcp", default-features = false }
anyhow = { workspace = true }
async-trait = { workspace = true }
tokio = { workspace = true }
```

**`GooseMcpMemoryAdapter`** implements `McpMemoryPort`:

```rust
pub struct GooseMcpMemoryAdapter {
    server: MemoryServer,  // goose_mcp::MemoryServer — Clone + all ops are sync fs I/O
    working_dir: Option<PathBuf>,
}

impl GooseMcpMemoryAdapter {
    pub fn new(memory_dir: PathBuf) -> Self {
        // MemoryServer::new() uses ~/.config/goose/memory/ by default;
        // override with custom dir for GIAP data isolation
    }
}
```

All `MemoryServer` methods are synchronous (`std::fs`). Wrap every call in `tokio::task::spawn_blocking`.

The `instructions()` method returns the full string that `MemoryServer` generates from stored memories — this is prepended to the LLM system prompt on every chat request.

**Tests (tempdir-based, fast):**
- `remember` → `retrieve` round-trips
- Wildcard `"*"` retrieves all categories
- `remove_category` clears files
- `instructions()` contains stored data after `remember`

### 2B. Wire MCP memory into `pond-server`

**File:** `crates/pond-server/Cargo.toml`
```toml
[features]
mcp-memory = ["dep:pond-adapters-mcp-memory"]

[dependencies]
pond-adapters-mcp-memory = { path = "../pond-adapters-mcp-memory", optional = true }
```

**File:** `crates/pond-server/src/main.rs`
```rust
#[cfg(feature = "mcp-memory")]
let mcp_memory: Option<Arc<dyn McpMemoryPort + Send + Sync>> = {
    use pond_adapters_mcp_memory::GooseMcpMemoryAdapter;
    Some(Arc::new(GooseMcpMemoryAdapter::new(data_dir.join("memory"))))
};
#[cfg(not(feature = "mcp-memory"))]
let mcp_memory: Option<Arc<dyn McpMemoryPort + Send + Sync>> = None;
```

### 2C. Memory injection into system prompt

**File:** `crates/pond-api/src/routes.rs` — in the chat handler:
```rust
let system = match &state.mcp_memory {
    Some(m) => format!("{BASE_SYSTEM}\n\n---\n## Remembered Context\n{}", m.instructions()),
    None => BASE_SYSTEM.to_string(),
};
```

**Verify Phase 2:**
```bash
cargo build -p pond-adapters-mcp-memory
cargo test -p pond-adapters-mcp-memory
cargo build -p pond-server --features mcp-memory
```

---

## Phase 3 — Local Inference (workspace-excluded, Jetson-ready)

This phase eliminates the llamafile external process entirely — model weights load in-process via `llama-cpp-2`.

### 3A. `pond-adapters-local-inference` crate

**Workspace `Cargo.toml` changes:**
```toml
exclude = [
    "crates/pond-adapters-goose",
    "crates/pond-adapters-mcp-memory",
    "crates/pond-adapters-local-inference",
]

pond-adapters-local-inference = { path = "crates/pond-adapters-local-inference" }
```

**`crates/pond-adapters-local-inference/Cargo.toml`:**
```toml
[features]
default = []
cuda = ["goose/cuda"]

[dependencies]
pond-core = { workspace = true }
# Reuse GooseProviderAdapter — message conversion is already correct there
pond-adapters-goose = { path = "../pond-adapters-goose" }
goose = { path = "../../goose/crates/goose", default-features = false,
          features = ["local-inference", "rustls-tls"] }
anyhow = { workspace = true }
async-trait = { workspace = true }
tokio = { workspace = true }
uuid = { workspace = true }
tracing = { workspace = true }
```

**`LocalInferenceLlmAdapter`:**

```rust
pub struct LocalInferenceLlmAdapter {
    inner: GooseProviderAdapter,  // handles ChatMessage ↔ goose Message conversion
}

impl LocalInferenceLlmAdapter {
    pub async fn new(model_id: &str) -> anyhow::Result<Self> {
        use goose::model::ModelConfig;
        use goose::providers::local_inference::LocalInferenceProvider;

        let model_config = ModelConfig {
            model_name: model_id.to_string(),
            ..Default::default()
        };
        let provider = LocalInferenceProvider::from_env(model_config, vec![]).await?;
        let session_id = uuid::Uuid::new_v4().to_string();
        Ok(Self {
            inner: GooseProviderAdapter::new(Arc::new(provider), session_id),
        })
    }
}

impl LlmProvider for LocalInferenceLlmAdapter { /* delegate to self.inner */ }

/// Default model for Jetson Orin Nano (3B Q4 ≈ 2.0 GB, fits in 8 GB with headroom)
pub const DEFAULT_LOCAL_MODEL: &str = "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M";
```

**How `LocalInferenceProvider` works internally:**
- `from_env(ModelConfig, extensions)` → calls `InferenceRuntime::get_or_init()` (global singleton, thread-safe `Weak` + `StdMutex`)
- Model weights are loaded on first `complete()` call via `load_model_sync()` → `LlamaModel::load_from_file()`
- Only `stream()` is implemented; `complete()` is a default method in the Goose `Provider` trait that collects the stream
- Metal (macOS) activated via `[target.'cfg(target_os = "macos")'.dependencies]` — no extra flag needed in dev
- CUDA (Jetson) activated via `--features cuda`

**Tests:** Unit tests with mock provider for message conversion; integration test `#[ignore]` requiring `$GIAP_TEST_MODEL_PATH`.

### 3B. `--provider local` CLI flag

**File:** `crates/pond-server/Cargo.toml`
```toml
[features]
local-inference = ["dep:pond-adapters-local-inference"]
mcp-memory      = ["dep:pond-adapters-mcp-memory"]

[dependencies]
pond-adapters-local-inference = { path = "../pond-adapters-local-inference", optional = true }
```

**File:** `crates/pond-server/src/main.rs` — extend `match provider`:
```rust
"local" => {
    #[cfg(feature = "local-inference")]
    {
        let model_id = /* --model flag or DEFAULT_LOCAL_MODEL */;
        let llm = Arc::new(LocalInferenceLlmAdapter::new(model_id).await?);
        chat_service = chat_service.with_provider(llm);
    }
    #[cfg(not(feature = "local-inference"))]
    eprintln!("--provider local requires: cargo build -p pond-server --features local-inference");
}
```

**Verify Phase 3:**
```bash
# macOS dev (Metal auto-activated)
cargo build -p pond-adapters-local-inference
cargo run -p pond-server --features local-inference -- chat --provider local

# Jetson Orin Nano (CUDA)
cargo build -p pond-server --features "local-inference,local-inference/cuda"
cargo run  -p pond-server --features "local-inference,local-inference/cuda" -- chat --provider local
```

---

## Summary of All File Changes

### New Crates

| Crate | Workspace? | Purpose |
|---|---|---|
| `crates/pond-infra-scheduler/` | **Included** | `CronSchedulerAdapter` implements `SchedulerPort` |
| `crates/pond-adapters-mcp-memory/` | Excluded | `GooseMcpMemoryAdapter` implements `McpMemoryPort` |
| `crates/pond-adapters-local-inference/` | Excluded | `LocalInferenceLlmAdapter` implements `LlmProvider` |

### Modified Files

| File | What changes |
|---|---|
| `Cargo.toml` (workspace) | Members, excludes, workspace deps (`pond-infra-scheduler`, `tokio-cron-scheduler`) |
| `crates/pond-core/src/user_data/ports/mod.rs` + `crates/pond-core/src/mcp/ports/mod.rs` | `pub mod scheduler;` (user_data) · `pub mod mcp_knowledge;` (mcp) |
| `crates/pond-core/src/shared/services/chat.rs` | `compactor` field + `with_context_compactor` builder |
| `crates/pond-api/src/lib.rs` | `mcp_memory` + `scheduler` fields in `AppState` |
| `crates/pond-api/src/routes.rs` | Schedule routes + memory injection in chat handler |
| `crates/pond-server/Cargo.toml` | Optional deps + feature flags |
| `crates/pond-server/src/main.rs` | Wire scheduler, mcp_memory, `--provider local` |

### New Files in `pond-core`

| File | Purpose |
|---|---|
| `src/ports/scheduler.rs` | `SchedulerPort` + `ScheduledTask` + `CreateTaskRequest` |
| `src/ports/mcp_memory.rs` | `McpMemoryPort` |
| `src/services/context_compactor.rs` | `ContextCompactor` (LLM-based compaction) |

---

## Key Reuse — Do Not Re-implement

| Existing component | Where | How it's reused |
|---|---|---|
| `GooseProviderAdapter` | `crates/pond-adapters-goose/src/provider_adapter.rs` | `pond-adapters-local-inference` takes a path dep on it — no message conversion duplication |
| `InferenceRuntime::get_or_init()` | `goose/crates/goose/src/providers/local_inference.rs:63` | Called by `LocalInferenceProvider::from_env()` — global singleton, thread-safe |
| `MemoryServer::new()` | `goose/crates/goose-mcp/src/` | Used directly in `GooseMcpMemoryAdapter` — no MCP transport layer needed |
| `trim_to_budget()` | `crates/pond-core/src/models/services/context_budget.rs` | Kept as hard fallback inside `ContextCompactor::compact()` |

---

## Risks

| Risk | Mitigation |
|---|---|
| rmcp conflict re-enters workspace | Never add excluded crates to `members`; keep `default-features = false` on goose dep |
| `code-mode` feature leakage | Verify with `cargo tree -p pond-adapters-local-inference` that `pctx_code_mode` is absent |
| ARM64 Jetson linker | Ensure `libstdc++`, `libgomp`, CUDA runtime in build image; use `--features cuda` |
| `InferenceRuntime` init race | Handled by Goose's `StdMutex<Weak<>>` pattern; always init from async context |
| `MemoryServer` sync I/O blocks async | Wrap every `MemoryServer` call in `tokio::task::spawn_blocking` |
