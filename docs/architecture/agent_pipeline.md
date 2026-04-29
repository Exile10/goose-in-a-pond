# Agent Pipeline Architecture

The GIAP agent pipeline wraps the main LLM with two configurable processing stages: a **pre-processor** (ToolAgent) that enriches the user's message with tool data, and a **post-processor** (AnswerReviewer) that validates answer quality before delivery.

Last updated: April 29, 2026

---

## Pipeline Overview

```
User message
    |
    v
[ToolAgent] -- pre-processor
    |  Classify message with LLM (same model, no swap)
    |  If tool needed: fetch data via MCP (Wikipedia, weather, memory, etc.)
    |  Inject tool result into the agent message
    |
    v
[Main LLM] -- GooseAdapter
    |  System prompt with skills, memory, device context
    |  Streams tokens via SSE (AgentStreamEvent::Text)
    |  ThoughtFilter strips thinking tokens from output
    |
    v
[AnswerReviewer] -- post-processor (configurable: off/on/auto)
    |  Adversarial critic evaluates completeness, accuracy, depth
    |  If score < threshold: sends critique back to LLM for revision
    |  Emits review_status and review_revision SSE events
    |
    v
User (via SSE stream or TTS)
```

---

## ToolAgent (Pre-Processor)

### Port Trait

**File:** `crates/pond-core/src/ports/tool_agent.rs`

```rust
#[async_trait]
pub trait ToolAgent: Send + Sync {
    async fn process(&self, message: &str) -> Result<Option<String>>;
}
```

- Returns `Some(augmented_message)` when a tool was used -- the message includes the original text + retrieved information + instructions for the LLM.
- Returns `None` when no tool was needed -- the original message passes through unchanged.

### Implementation: `GiapToolAgent`

**File:** `crates/pond-server/src/main.rs`

The classifier uses the **live provider** (via `RwLock`) so it always runs on whatever model is currently loaded -- zero model swap overhead.

**Classification flow:**
1. Build classifier prompt from `build_classifier_prompt()` (cached via `OnceLock`)
2. Call `provider.complete()` with the classifier system prompt
3. Strip thinking tokens (`<|channel>thought...<channel|>`, `<think>...</think>`)
4. Extract JSON from response: `{"needs_tool": true, "tool": "wikipedia"}`
5. Retry up to 3 times if JSON parsing fails
6. If tool needed: dispatch to `pond_mcp_server::try_tool_agent(tool, message)`
7. Return augmented message with tool result injected

### Available Tools

| Tool ID | Trigger | Data Source |
|---------|---------|-------------|
| `wikipedia` | Factual/encyclopedic questions, comparisons, definitions | MediaWiki API |
| `weather` | Current weather, temperature, forecast | Open-Meteo API |
| `save_memory` | "remember", "don't forget", personal preferences | SQLite memory store |
| `recall_memory` | "do you remember", past preferences | SQLite memory store |
| `devices` | Device status, home automation | SQLite device registry |
| `schedules` | Scheduled tasks, reminders | SQLite scheduler |

### Classifier Prompt

The classifier prompt includes:
- Tool definitions with descriptions
- 26 few-shot examples covering all tool types
- Explicit instruction to prefer tool use for factual questions
- "When in doubt, USE THE TOOL" guidance for small models

The prompt is **cached** via `OnceLock` -- built once, reused for all subsequent calls.

---

## AnswerReviewer (Post-Processor)

### Port Trait

**File:** `crates/pond-core/src/ports/answer_reviewer.rs`

```rust
#[async_trait]
pub trait AnswerReviewer: Send + Sync {
    async fn review(
        &self,
        question: &str,
        answer: &str,
        tool_context: Option<&str>,
    ) -> Result<ReviewResult>;
}

pub struct ReviewVerdict {
    pub pass: bool,          // Quality bar met?
    pub score: u8,           // 1-5 rubric score
    pub expectations: Vec<String>,  // What the answer should contain
    pub critique: String,    // What's missing or wrong
}

pub struct ReviewResult {
    pub final_answer: String,  // Original or revised
    pub was_revised: bool,
    pub verdict: ReviewVerdict,
    pub rounds: u32,
}
```

### Settings

| Setting | Values | Default | Effect |
|---------|--------|---------|--------|
| `review_mode` | `"off"`, `"on"`, `"auto"` | `"off"` | When to review |
| `review_max_rounds` | 1-2 | 1 | Max review-revision cycles |
| `review_pass_threshold` | 1-5 | 3 | Minimum score to pass |

- `"off"` -- no review, zero overhead
- `"on"` -- every answer reviewed
- `"auto"` -- only review Think-classified or tool-augmented answers

### Review Loop

1. Reviewer evaluates answer with adversarial system prompt (rubric-based scoring)
2. Output: JSON verdict `{pass, score, expectations, critique}`
3. If `pass == false && score < threshold`: critique sent to main LLM for revision
4. Revised answer replaces original via `review_revision` SSE event
5. Graceful degradation: unparseable JSON defaults to `pass: true`

---

## SSE Event Types

| Event Type | Stage | Content |
|------------|-------|---------|
| `status` | Any | Progress text ("Thinking...", "Using tool...") |
| `thinking` | Main LLM | Chain-of-thought reasoning (when `show_thinking` enabled) |
| `tool_call` | ToolAgent | Tool name + arguments |
| `tool_result` | ToolAgent | Tool execution result |
| `text` | Main LLM | Streamed response tokens |
| `review_status` | AnswerReviewer | "Reviewing answer...", "Answer verified (score: N/5)" |
| `review_revision` | AnswerReviewer | Revised answer text + score + rounds |
| `done` | Final | Session ID, model role, usage stats |
| `error` | Any | Error message |

---

## Integration Points

### HTTP Path (`crates/pond-api/src/routes.rs`)

```
chat_stream handler:
  1. Load settings, build system prompt
  2. ToolAgent: state.tool_agent.process(&message)  [pre-processor]
  3. Agent: state.agent.chat_stream(request)         [main LLM]
  4. ThoughtFilter: strip thinking tokens from stream
  5. AnswerReviewer: state.answer_reviewer.review()   [post-processor]
  6. Persist final answer to session storage
  7. Emit done event
```

### Voice CLI Path (`crates/pond-core/src/services/chat.rs`)

Same pipeline in `chat_stream_once()`, with TTS output instead of SSE.

### Wiring (`crates/pond-server/src/main.rs`)

Both `GiapToolAgent` and `GiapAnswerReviewer` are injected into `AppState` (HTTP) and `ChatService` (voice CLI) at startup.

---

## Related Documents

- [Components](./components.md) -- port trait definitions
- [Data Flow](./data_flow.md) -- request lifecycle
- [Model Capabilities](./model_capabilities.md) -- how thinking mode affects the pipeline
- [Inference Optimization](../developer/inference_optimization.md) -- classifier performance tuning
