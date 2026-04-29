# Inference Optimization Guide

Performance tuning for GIAP's on-device LLM inference. Covers hardware-specific settings, context management, classifier overhead reduction, and memory pressure mitigation.

Last updated: April 29, 2026

---

## Platform-Aware Model Settings

**File:** `crates/pond-adapters-local-inference/src/lib.rs`

GIAP automatically applies optimized `ModelSettings` based on the build target. Without these, inference defaults to CPU-only on all platforms.

### macOS / Metal (`#[cfg(not(feature = "cuda"))]`)

`apply_platform_settings()` -- called on every model initialization:

| Parameter | Value | Why |
|-----------|-------|-----|
| `n_gpu_layers` | 99 | Full Metal GPU offload (Apple Silicon unified memory) |
| `context_size` | 8192 | Stable 8K context -- fits ~6K history + 2K generation |
| `n_batch` | 512 | Optimal Metal prefill throughput |
| `flash_attention` | true | ~40% KV cache reduction |
| `use_mlock` | false | Unified memory -- mlock unnecessary |
| `n_threads` | auto | llama.cpp auto-detects (good on Apple Silicon) |

**Impact:** Without these settings, `n_gpu_layers` defaults to `None` (CPU-only inference). Enabling Metal offload gives **5-10x faster prefill, 3-5x faster generation** on M1-M4.

### Jetson Orin Nano / CUDA (`#[cfg(feature = "cuda")]`)

`apply_jetson_settings()`:

| Parameter | Value | Why |
|-----------|-------|-----|
| `n_gpu_layers` | 99 | Full GPU offload (8GB unified DRAM) |
| `context_size` | 3072 | Safe for 8GB total system RAM |
| `n_batch` | 512 | Ampere SM saturation |
| `n_threads` | 4 | 4 of 6 A78AE cores (spare for OS + voice) |
| `flash_attention` | true | Native Ampere support |
| `use_mlock` | false | Unified memory -- mlock causes page faults |

---

## Context Window Management

### Dynamic Context Budgeting

**File:** `crates/pond-core/src/services/context_budget.rs`

```rust
// Model-aware trimming (uses reported context window)
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

## Classifier Overhead Reduction

### Live Provider (No Model Swap)

**File:** `crates/pond-server/src/main.rs`

The tool classifier reads from the live `RwLock<Option<Arc<dyn LlmProvider>>>` instead of a static Arc captured at startup. This means:

- Classifier always uses whatever model is currently loaded
- Hot-reloading the model via UI updates the classifier automatically
- **Zero model unload/reload overhead** per message

### Thinking Token Stripping

`strip_thinking_from_classifier()` handles Gemma 4 and Qwen3 thinking preambles:

1. Strips `<|channel>thought...<channel|>` blocks (Gemma 4)
2. Strips `<think>...</think>` blocks (Qwen3/DeepSeek)
3. Extracts JSON by finding first `{` and last `}`
4. Retries up to 3 times if no valid JSON produced

### Prompt Caching

| What | Before | After |
|------|--------|-------|
| `build_classifier_prompt()` | 3KB string allocated per call | `OnceLock` -- built once, reused forever |
| `giap_tool_definitions()` | `Vec<>` heap allocation | `&'static [...]` zero-cost slice |
| `giap_tool_description_lines()` | 6 `format!()` per turn | `OnceLock` -- cached after first call |

---

## Memory Pressure Sources

### Critical (Fixed)

| Source | Impact | Fix |
|--------|--------|-----|
| Classifier loading different model | ~200MB model swap per message | Live provider via RwLock |
| Classifier prompt rebuilt per call | 3KB x 3 retries = 9KB/message | OnceLock caching |
| Tool descriptions rebuilt per turn | 6 format!() allocations | Static cached slice |

### Known (Goose Upstream)

| Source | Impact | Status |
|--------|--------|--------|
| LlamaContext created per inference call | 50-200MB KV cache allocation | Goose core design -- mitigated by fixed `context_size` |
| Full message history sent every turn | Grows with conversation | Managed by `trim_to_budget_for_model()` |
| `goose_session_map` never shrinks | ~128 bytes per session | Low priority -- slow leak |

---

## Settings Reference

| Setting | Key | Default | Range |
|---------|-----|---------|-------|
| Max tokens | `llm_max_tokens` | 4096 | 256-16384 |
| Temperature | `llm_temperature` | 0.7 | 0.0-2.0 |
| Thinking mode | `thinking_mode` | "auto" | "auto", "on", "off" |
| Context override | `context_window_override` | 0 (auto) | 0-131072 |

---

## Related Documents

- [Model Capabilities](../architecture/model_capabilities.md) -- how context window is reported
- [Agent Pipeline](../architecture/agent_pipeline.md) -- classifier in the pipeline
- [Data Pipeline](../architecture/data_pipeline.md) -- Jetson hardware constraints
