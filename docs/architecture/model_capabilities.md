# Model Capabilities System

Runtime capability discovery for LLM providers. Each adapter reports what the active model supports (thinking, vision, context window, etc.), and services adapt behavior accordingly -- without any model-specific code in `pond-core`.

Last updated: April 29, 2026

---

## ModelCapabilities Struct

**File:** `crates/pond-core/src/models/domain/model_capabilities.rs`

```rust
pub struct ModelCapabilities {
    pub thinking: bool,              // Chain-of-thought reasoning (Gemma 4, Qwen3, DeepSeek-R1)
    pub vision: bool,                // Accepts image input (Gemma 4, LLaVA)
    pub audio_input: bool,           // Accepts raw audio (Gemma 4 E2B/E4B)
    pub context_window_tokens: u32,  // Max context window (default: 4096)
    pub structured_output: bool,     // GBNF grammar / JSON mode (GGUF models)
    pub tool_calling: bool,          // Native tool calling (e.g. Gemma 4 `<|tool_call>`)
}
```

When `tool_calling` is true, tool definitions are passed through the chat template; when false, tools are described in the system prompt text instead.

All fields default to the most conservative values (`false` / `4096`) so unknown models work safely.

---

## Detection: `from_model_name()`

Heuristic detection from model identifier strings:

| Pattern | thinking | vision | audio | context | structured |
|---------|----------|--------|-------|---------|------------|
| `gemma-4` / `gemma4` | yes | yes | E2B/E4B only | 128K | yes (GGUF) |
| `qwen3` / `qwq` | yes | no | no | 32K | no |
| `deepseek-r1` | yes | no | no | 4K | no |
| `llava` / `bakllava` | no | yes | no | 4K | no |
| `llama-3` / `llama3` | no | no | no | 8K | no |
| `mistral` | no | no | no | 32K | no |
| `*.gguf` / `q4_k` etc. | no | no | no | 4K | yes |

---

## Trait Integration

### LlmProvider

**File:** `crates/pond-core/src/models/ports/provider.rs`

```rust
pub trait LlmProvider: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()  // safe default
    }
    // ... complete(), stream_complete(), model_name()
}
```

### Agent

**File:** `crates/pond-core/src/models/ports/agent.rs`

```rust
pub trait Agent: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    // ... chat(), chat_stream()
}
```

Default implementations return conservative values. Adapters override with model-specific detection.

---

## Adapter Implementations

| Adapter | How capabilities are resolved |
|---------|-------------------------------|
| GooseAdapter | `from_model_name()` called in `ensure_provider_current()` after model swap. Stored in `Mutex<ModelCapabilities>`. |
| OllamaAdapter | `from_model_name()` on the configured model string. |
| LocalInferenceLlmAdapter | `from_model_name()` on `inner.model_name()`. |
| GooseProviderAdapter | `from_model_name()` on provider's model config name. |

---

## What Capabilities Drive

### Thinking Mode (`thinking: true`)

- `PromptState.thinking_enabled` activates "Deep Thinking" section in system prompt
- GooseAdapter resolves: `settings.thinking_mode == "auto" && caps.thinking`
- ThoughtFilter captures thinking blocks as SSE events (when `show_thinking` enabled)
- Adaptive response budget: complex queries get `base_max_tokens * 2`

### Context Window (`context_window_tokens`)

- `trim_to_budget_for_model()` uses the model's reported window instead of hardcoded 12K
- `context_window_override` setting caps the value for memory-constrained deployments
- 128K-capable models retain 10-30x more conversation history

### Vision (`vision: true`)

- `ChatMessage.images: Vec<ImageAttachment>` carries base64-encoded images
- GooseAdapter attaches images via `Message::with_image()` when vision is true
- Frontend shows image upload button only when `capabilities.vision == true`
- Camera snapshots can be attached when `vision == true`

---

## REST API

### `GET /api/v1/models/capabilities`

Returns the active model's capabilities as JSON:

```json
{
  "thinking": true,
  "vision": true,
  "audio_input": false,
  "context_window_tokens": 128000,
  "structured_output": true
}
```

### Frontend

- **Models page:** Capability badges on model cards (Thinking, Vision, Audio, 128k ctx)
- **Active Roles banner:** "Model features" row showing active model's capabilities
- **Settings page:** Thinking Mode toggle (auto/on/off)

---

## Related Documents

- [Agent Pipeline](./agent_pipeline.md) -- how capabilities affect the processing pipeline
- [Components](./components.md) -- port trait definitions
- [Inference Optimization](../developer/inference_optimization.md) -- platform-specific settings
