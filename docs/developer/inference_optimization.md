# Inference Optimization Guide

Performance tuning for GIAP's on-device LLM inference. Covers the LlamaCppEngine, KV-cache persistence, capabilities caching, context management, and Gemma 4 tool-call argument handling.

Last updated: May 18, 2026

---

## LlamaCppEngine Architecture

**File:** `crates/pond-inference/src/engine.rs`

GIAP's inference engine wraps llama-cpp-2 directly, with no Goose dependency. The engine manages a global `LlamaBackend` singleton, model loading/unloading, and persistent KV-cache contexts.

### Key types

```rust
// engine.rs:105
pub(crate) type ModelSlot = Arc<Mutex<Option<LoadedModel>>>;

// engine.rs:114
pub struct LlamaCppEngine {
    model: ModelSlot,
    backend: Arc<LlamaBackend>,
    data_dir: PathBuf,
    capabilities: Arc<StdRwLock<ModelCapabilities>>,  // lock-free read cache
}
```

### Backend singleton

`get_or_init_backend()` (engine.rs:71) uses a `StdMutex<Weak<LlamaBackend>>` global. If the backend was already initialized by Goose's `InferenceRuntime` in the same process, the function wraps the existing C-level backend rather than failing.

---

## Lock-Free Capabilities Cache

**File:** `crates/pond-inference/src/engine.rs:121`

### The problem (old try_lock race)

The model `Mutex` is held for the entire duration of generation (5-60 seconds). Any caller using `model.try_lock()` to read capabilities during generation would get `None` and silently return `ModelCapabilities::default()` -- which has `tool_calling: false`. This meant tools were disabled whenever inference was running.

### The fix

`LlamaCppEngine` maintains a separate `Arc<StdRwLock<ModelCapabilities>>` that is updated atomically on model load/unload:

```rust
// engine.rs:167 — inside load_model()
*self.capabilities.write().expect("capabilities lock poisoned") =
    loaded.capabilities.clone();

// engine.rs:224 — lock-free read, safe from sync contexts
pub fn cached_capabilities(&self) -> ModelCapabilities {
    self.capabilities
        .read()
        .expect("capabilities lock poisoned")
        .clone()
}
```

The `InferenceProvider::capabilities()` impl in provider.rs:76 calls `cached_capabilities()` instead of `try_lock()`:

```rust
fn capabilities(&self) -> ModelCapabilities {
    // Uses lock-free cache — never contends with model mutex.
    self.cached_capabilities()
}
```

### Why `StdRwLock` (not tokio)

`capabilities()` is a sync trait method. Using `std::sync::RwLock` avoids requiring async contexts. The write side is called only during `load_model`/`unload_model` (rare), so contention is effectively zero.

---

## KV-Cache Persistence (In-Memory)

**File:** `crates/pond-inference/src/provider.rs:93`, `crates/pond-inference/src/kv_cache.rs`

### How it works

After each generation, the llama.cpp context and its token log are stored back into the `LoadedModel` struct (provider.rs:545):

```rust
loaded.cached_ctx = Some(CachedInferenceContext {
    ctx,
    tokens_in_cache: tokens,
});
```

On the next turn, `plan_cache_reuse()` (kv_cache.rs:70) compares the cached tokens against the new prompt tokens:

| Result | Action | Savings |
|--------|--------|---------|
| `FullHit` | Skip all prefill | ~2000 tokens (system prompt + tools) |
| `IncrementalDecode` | Trim stale, decode delta only | Proportional to prefix length |
| `FullPrefill` | Create fresh context | 0 (cold start) |

On Jetson, this saves 5-15 seconds per turn by not re-prefilling the stable system prompt + tool declarations.

### Context resizing

If the cached context is too small for the current prompt (conversation grew), the engine creates a fresh context with a larger `n_ctx` (provider.rs:283):

```rust
if tokens.len() + 512 > cached_n_ctx {
    tracing::info!("KV cache too small for current prompt -- creating larger context");
    loaded.cached_ctx = None;
    // create_fresh_ctx with effective_context_size()
}
```

### Decode failure recovery

If a decode fails (e.g., `NoKvCacheSlot`), the engine drops the cached context and retries with a full prefill (provider.rs:367-397).

---

## Gemma 4 Native Argument Format

**File:** `crates/pond-inference/src/tool_calling.rs:427`

Gemma 4 emits tool-call arguments in a non-standard format with `<|"|>` string delimiters and unquoted keys:

```
Input:  {location:<|"|>Athens<|"|>}
Output: {"location":"Athens"}
```

`gemma4_args_to_json()` normalizes this in two steps:
1. Replace `<|"|>` with `"` (delimiter swap)
2. Quote unquoted keys (walk characters, insert `"` around identifiers before `:`)

The function passes through already-valid JSON unchanged (starts with `{"` or equals `{}`).

### Tool call format detection

`parse_tool_calls()` (tool_calling.rs:80) tries three formats in order:
1. Llama3/Gemma4: `<|tool_call|>call:NAME{...}<tool_call|>` or `<|tool_call>call:NAME{...}<tool_call|>`
2. Qwen/generic XML: `<tool_call><function=NAME>...</function></tool_call>`
3. Standard JSON: `{"tool_calls": [...]}`

---

## GIAP_DUMP_PROMPT Debugging

**File:** `crates/pond-inference/src/provider.rs:168`

Set `GIAP_DUMP_PROMPT` to dump the fully-rendered prompt (after Jinja template application) to a file. This shows exactly what the model receives: system prompt, tool declarations, and user messages.

```bash
# Dump to /tmp/giap-rendered-prompt.txt
GIAP_DUMP_PROMPT=1 cargo run -p pond-server -- serve

# Dump to a custom path
GIAP_DUMP_PROMPT=/tmp/my-prompt.txt cargo run -p pond-server -- serve
```

The dump includes the complete Jinja-rendered output including tool schemas, thinking configuration, and all message content. Useful for verifying that tools are formatted correctly for the model.

---

## tools_json_override Passthrough

**File:** `crates/pond-inference/src/provider.rs:123`, `crates/pond-core/src/models/ports/inference.rs:51`

The `InferenceOptions` struct has two override fields:

```rust
pub struct InferenceOptions {
    // ...
    pub tools_json_override: Option<String>,         // full schemas
    pub compact_tools_json_override: Option<String>,  // name+desc only
}
```

When set, the provider passes the pre-formatted JSON directly to `apply_chat_template_oaicompat()` instead of re-serializing `ToolDefinition` objects. This is populated by `PondAgent` from `ToolDispatcher::tools_json()` (tool_dispatcher.rs:60), which produces the exact OpenAI-compatible format that Goose's `format_tools()` generates.

The fallback chain in `generation_task()` (provider.rs:123-145):

1. `tools_json_override` -- pre-formatted from dispatcher (preferred)
2. `tools_to_json(&tools)` -- re-serialize from `ToolDefinition` objects
3. Skip on small context (n_ctx_train <= 4096) -- go straight to compact
4. `compact_tools_json` -- name + description only (budget fallback)

---

## Template Rendering Pipeline

**File:** `crates/pond-inference/src/provider.rs:571`

`apply_template()` renders the prompt through the model's Jinja chat template with a full-then-compact fallback:

1. Try full tool schemas via `apply_chat_template_oaicompat()`
2. Tokenize the result -- if it exceeds `n_ctx_train - 512`, fall back to compact
3. If the full template fails entirely, try compact schemas
4. If both fail, return error

The function returns a `ChatTemplateResult` containing:
- `prompt` -- the rendered text
- `additional_stops` -- extra stop sequences from the template
- `grammar` / `grammar_triggers` -- GBNF constraints (currently unused; Gemma 4 handles tool formatting via fine-tuning)

---

## Context Window Estimation

**File:** `crates/pond-inference/src/memory.rs`

`effective_context_size()` determines the context window for each inference call:

1. **Memory-based**: Queries GPU/accelerator free memory, reads KV cache dimensions from GGUF metadata (head_dim, n_kv_heads, n_layer), reserves 50% for compute scratch
2. **Model-based**: Reads `n_ctx_train` (training context length)
3. **Result**: `min(memory_estimate, n_ctx_train)`, with 512 tokens reserved for generation headroom

On Jetson with 8GB unified RAM, this typically caps at 4096 for E4B-class models.

---

## Platform-Aware Settings

When constructing inference contexts, `LlamaCppEngine` applies these parameters:

### macOS / Metal

| Parameter | Value | Why |
|-----------|-------|-----|
| `n_gpu_layers` | 99 | Full Metal GPU offload (Apple Silicon unified memory) |
| `n_batch` | 512 | Optimal Metal prefill throughput |
| `flash_attention` | enabled (policy=1) | ~40% KV cache reduction |

### Jetson Orin Nano / CUDA

| Parameter | Value | Why |
|-----------|-------|-----|
| `n_gpu_layers` | 99 | Full GPU offload (8GB unified DRAM) |
| `n_batch` | 512 | Ampere SM saturation |
| `flash_attention` | enabled (policy=1) | Native Ampere support |

Parameters are set at context creation time in `generation_task()` (provider.rs:253-256).

---

## Context Window Management

### Dynamic Context Budgeting

**File:** `crates/pond-core/src/models/services/context_budget.rs`

```rust
pub fn trim_to_budget_for_model(
    messages: Vec<ChatMessage>,
    capabilities: &ModelCapabilities,
    override_tokens: u32,  // 0 = use model's value
) -> Vec<ChatMessage>
```

- Reserves 20% for system prompt + generation headroom (min 2048 tokens)
- `context_window_override` setting caps the window for memory-constrained deployments
- Falls back to `trim_to_budget()` (hardcoded 12K chars) for unknown models

### Adaptive Response Budget

**File:** `crates/pond-core/src/prompts.rs`

```rust
pub fn estimate_response_budget(message: &str, base_max_tokens: u32) -> u32
```

| Query Type | Token Budget | Example |
|------------|-------------|---------|
| Short (< 25 chars, non-complex) | `base / 2` (2048) | "hi", "thanks" |
| Complex (explain, compare, plan) | `base * 2` (8192) | "explain photosynthesis step by step" |
| Normal | `base` (4096) | "what's the weather?" |

---

## Memory Pressure Sources

### Mitigated

| Source | Impact | Fix |
|--------|--------|-----|
| Context recreated per call | 50-200MB KV cache allocation | In-memory KV-cache persistence (provider.rs:545) |
| Capabilities returned default during generation | Tools silently disabled | Lock-free `StdRwLock` capabilities cache (engine.rs:121) |
| Full prompt re-tokenized every turn | 10-20s wasted on Jetson | Prefix matching via `plan_cache_reuse()` (kv_cache.rs) |

### Known

| Source | Impact | Status |
|--------|--------|--------|
| Full message history sent every turn | Grows with conversation | Managed by `trim_to_budget_for_model()` and PondAgent history budget (4000 chars) |
| Grammar sampler crashes with Gemma 4 GBNF | Cannot enforce tool-call structure | Disabled; model fine-tuning handles formatting |
| Backend singleton conflict (Goose + Pond) | Cannot run both engines in one process | `agent_backend` setting selects one engine |

---

## Settings Reference

| Setting | Key | Default | Range |
|---------|-----|---------|-------|
| Max tokens | `llm_max_tokens` | 4096 | 256-16384 |
| Temperature | `llm_temperature` | 0.7 | 0.0-2.0 |
| Thinking mode | `thinking_mode` | "auto" | "auto", "on", "off" |
| Context override | `context_window_override` | 0 (auto) | 0-131072 |
| Agent backend | `agent_backend` | "goose" | "goose", "pond" |

---

## Memory-Fit Guard (Phase 6)

On-device decode is **memory-bandwidth-bound** (~102 GB/s on the Jetson Orin
Nano). When a model fully resides in the unified-memory GPU budget, throughput
is roughly `tok/s ≈ 102 / model_size_GB`. When the model exceeds the budget it
**silently partial-offloads to CPU** and decode collapses to single-digit tok/s.

This bit users with `gemma3n:e2b`: the Ollama download is really **~5.6 GB**,
not the 3.1 GB the UI once estimated. On 8 GB it cannot fit alongside the OS,
STT, TTS, and KV cache, so it spilled to CPU and felt "hella slow".

### What the guard does (shipped, cross-platform)

- **Decision helper** — `pond-desktop/src/api/modelFit.ts` (`modelFit`,
  `modelFitFor`): pure `fits`/`spills`/`unknown` verdict given a model's
  residency size and the LLM budget from `GET /api/v1/models/memory-status`,
  reserving `DEFAULT_HEADROOM_MB = 1024` for KV cache + system slack. Unit-tested
  (`modelFit.test.ts`). The server mirrors this in `model_spills_budget`
  (`crates/pond-api/src/routes.rs`) with the same `MEMORY_FIT_HEADROOM_MB = 1024`.
- **UI warning** — a shared `FitBadge` (lucide `AlertTriangle`, no emoji) shows
  "Too large — will spill to CPU and run slowly" on models that exceed the budget
  on *this* device, in both onboarding (`StepModel`) and the Models Manage tab
  (`sections/Models`). It stays quiet for `fits` and `unknown`, so on Mac/dev
  (NoopScheduler / `total_mb == 0`) nothing is shown.
- **Server log** — `warn_if_model_spills` logs a `tracing::warn!` at LLM-role
  activation when the model exceeds the budget. Log only; no loader change.

### Recommended on-Jetson fail-closed enforcement (NOT yet implemented)

The loader (`ModelSettings { n_gpu_layers: 99, .. }` in
`crates/pond-adapters-local-inference/src/lib.rs::apply_jetson_settings`) still
requests full GPU residency and silently splits when it does not fit. When it can
be validated on real Jetson hardware, enforce fail-closed at load:

1. Before `-ngl 99`, compare the model's on-disk size against `MemAvailable`
   (`ResourceAwareModelScheduler::memory_status`), reserving ~1 GB headroom.
2. If it will not fit, run `echo 3 > /proc/sys/vm/drop_caches` (root) first to
   free the page cache — otherwise the `-ngl` allocation hits the NvMap OOM wall
   (error 12). See `scripts/jetson-llama-optimization`.
3. Re-check; if it STILL will not fit, refuse the full-GPU load (fail closed) and
   surface the spill to the UI rather than silently degrading to a CPU/GPU split.

This is deliberately kept out of the cross-platform loader — it is unsafe to
change from the macOS Metal build and is not testable there.

---

## Related Documents

- [Debug Guide](debug_guide.md) -- GIAP_DUMP_PROMPT and log-level debugging
- [Goose Integrations](goose_integrations.md) -- dual-engine architecture
- [Data Pipeline](../architecture/data_pipeline.md) -- Jetson hardware constraints
