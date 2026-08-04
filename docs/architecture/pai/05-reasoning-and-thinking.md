# PAI-5 — Reasoning and thinking

Requirement: *the ability of the model to think.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-3](./03-context-governor.md) and [PAI-4](./04-smart-compaction.md) — reasoning
tokens are what overrun the window, so they cannot be budgeted before the budget is honest.

Verified against code 2026-08-03.

---

## 1. What is true today

### 1.1 Thinking is configured in three places and produced in none

**Settings.** `thinking_mode: "auto" | "on" | "off"` (default `auto`, `settings.rs:374-379`) and
`show_thinking: bool` (default false, `:381-384`).

**Capability.** `ModelCapabilities.thinking` (`model_capabilities.rs:14-17`) set by
`from_model_name` for `gemma-4`, `qwen3`, `qwq`, `deepseek-r1` (`:154-168`).

**Resolution.** `thinking_section_applies(mode, model, voice)` (`goose_agent.rs:1012-1026`) — voice
forces false, `on`/`off` are explicit, `auto` consults the model name. The doc comment
(`:1000-1011`) explains it uses the *name* rather than the capability cache because the cache is
stale on turn one, and the resulting 78-character prompt delta moved `prefix_hash` and cost every
session a full re-prefill on its second turn — 3.7 s on the Orin. That reasoning is sound and stays.

**Engine parameter.** `with_thinking_param` (`:1062-1074`) merges `{"enable_thinking": bool}` into
`ModelConfig.request_params`, only for `local`/`gguf`, tracked by `last_thinking_param`
(`:208-211`) so a mode change re-stamps without a provider rebuild.

### 1.2 `AgentStreamEvent::Thinking` has zero producers

Four references exist in `crates/`, all consumers:

| Site | What it does |
|---|---|
| `shared/services/chat.rs:1084` | matches it and **ignores** it |
| `pond-api/src/routes.rs:1415` | forwards as a `thinking` SSE event |
| `pond-api/src/routes.rs:7180` | same, on `/agent/chat/stream` |
| `pond-server/src/main.rs:6178` | CLI, prints dimmed to stderr |

No adapter ever emits it. Thinking reaches the UI **only** through `ThoughtFilter`, a streaming state
machine (`pond-api/src/thought_filter.rs`, 511 lines) that scrapes tag-delimited blocks out of the
text stream: `<|channel>thought…<channel|>`, `<think>…</think>`, `<thought>…</thought>`
(`:23-28`), plus standalone sentinels (`:33-40`). It is constructed with capture only when
`show_thinking && !voice_mode` (`routes.rs:1374-1378`).

So the entire feature rests on regex-matching whatever envelope the model happens to emit.

### 1.3 The structured reasoning channel is parsed and thrown away

`pond-inference/src/provider.rs:420-422` documents that `ChatParseStateOaicompat` "extracts
structured deltas (content, reasoning_content, tool_calls)". The delta-consumption loop
(`:493-511`) reads `delta["content"]` and `delta["tool_calls"]`. **`reasoning_content` appears
exactly once in all of `crates/` — in that comment.** llama.cpp has already separated the reasoning
for us and we discard it, then re-derive it downstream with string matching.

`ChatEvent` (`models/ports/inference.rs:17-28`) is `Text | ToolCall | Usage`. There is no reasoning
variant at the port level, so even a willing adapter has nowhere to put it.

### 1.4 There is no reasoning budget and no reasoning accounting

`grep -rni reasoning_effort crates/` returns nothing. No thinking-token budget, no interleaved or
extended thinking support. `UsageStats` carries prompt and completion only, so **reasoning tokens
bill as completion tokens** and are invisible to every budget calculation.

That invisibility is not academic. `context_budget.rs:43-56` records the exact failure: on the Orin
at `n_ctx 4096`, a thinking block of up to 306 tokens overruns the window *mid-generation*, llama.cpp
returns `ContextLengthExceeded`, and Goose compacts reactively on a path GIAP cannot suppress. The
user's history is replaced by a summary and their answer truncated to one character.
`output_reserve_tokens` exists solely to absorb this, as a fixed constant, because nothing measures
the real thing.

### 1.5 Reasoning is never persisted

Thinking blocks are stripped before `full_text` accumulates, so nothing reaches `session_messages`.
Reopening a conversation shows the answers and none of the working.

### 1.6 `/agent/chat/stream` is missing most of the pipeline

It builds a bare `ThoughtFilter` (`routes.rs:7171`), so `show_thinking` is a no-op there. It also has
no idle timeout, no answer review, no telemetry, no context monitoring, no `turn_stats`, no usage in
its `done` payload (`:7223-7225`), and it calls plain `persist_assistant_turn` rather than
`_with_extraction` — **so no memories are extracted from anything that route handles.**

### 1.7 The prompt/engine inconsistency is already fixed

I originally recorded this as an outstanding bug, on the strength of
`context-and-reasoning-roadmap.md`. Re-checked 2026-08-04: **it is fixed, and that roadmap is the
stale source.**

One value drives both consumers. `goose_agent.rs` computes
`let thinking_enabled = Self::thinking_section_applies(...)` once, hoisted specifically so it can
feed `PromptState { thinking_enabled, .. }` *and* `ensure_provider_current(.., thinking_enabled)`,
which stamps the engine's `enable_thinking` request-param. The comment above it says so outright:
"Hoisted out of the PromptState block below because it now drives two things that must agree."

The roadmap's neighbouring premise — that "engine-level `enable_thinking` is a registry default GIAP
never sets" — is also no longer true.

The one residual: `with_thinking_param` early-returns for providers other than `local`/`gguf`. That
is deliberate and documented, and is a different thing from the roadmap's complaint.

---

## 2. The gap

GIAP has a thinking *display* feature built on string matching, and no thinking *capability*
feature. The model reasons, the engine separates the reasoning, GIAP throws it away, scrapes it back
out of the prose, shows it if asked, forgets it, and never counts it — while the uncounted tokens are
the single documented cause of mid-generation context overrun on the target hardware.

---

## 3. Design

### 3.1 A first-class reasoning channel

Add to `models/ports/inference.rs`:

```rust
pub enum ChatEvent {
    Text(String),
    Reasoning(String),          // new
    ToolCall { /* … */ },
    Usage(UsageStats),
}
```

and read `delta["reasoning_content"]` in `pond-inference/src/provider.rs` alongside `content` and
`tool_calls`. The parsing already happened; this is claiming a value that is sitting in a map.

`GooseAdapter` becomes a real producer of `AgentStreamEvent::Thinking`. `ThoughtFilter` is
**demoted, not deleted** — it remains the correct mechanism for models that inline their reasoning
as tags in the content stream, which is most of them today. The order becomes: structured channel if
the provider offers one, tag scraping otherwise.

This matters beyond tidiness. A structured channel cannot be confused with prose, cannot be truncated
mid-tag by a chunk boundary, and does not need the lookahead buffering `ThoughtFilter` performs on
every token.

### 3.2 Count reasoning tokens

`UsageStats` gains `reasoning_tokens: Option<u32>`, persisted alongside the existing per-message
counts (the next free migration number — `0037` at the time of writing — extending `0029`'s
columns). This closes the loop with PAI-3: the governor
can then set `output_reserve_tokens` from **measured** reasoning behaviour for the active model
rather than from a constant chosen to survive the worst case observed once on an Orin.

The cost model also becomes honest — `GET /usage/summary` currently attributes reasoning to
completion, which overstates answer length and understates why the device felt slow.

### 3.3 A thinking budget, derived not typed

```
reasoning_effort = "brief" | "balanced" | "thorough"     # default "brief" on local providers
```

Mapped to a concrete token budget by the compaction profile rather than entered as a raw number,
because the right value is a function of the window and the device, not a preference. On-device
default is `brief` for a blunt reason: reasoning tokens are decode tokens, and decode on the Orin is
memory-bandwidth-bound at roughly `102 / model_GB` tok/s. A 300-token thinking block on a 2 GB model
is about fourteen seconds of silence before the answer starts.

`thorough` stays available and is the right default for HTTP providers, where the tokens are cheap
and fast.

Where the budget is exceeded mid-generation, the engine should be asked to stop reasoning and answer
— which is a better failure than the current one (overrun, reactive compaction, truncated answer).

### 3.4 Persist reasoning, but do not replay it

`persist_thinking: bool` (default off). When on, reasoning is stored in a side table keyed by message
id — not as a `session_messages` role, because it must never be picked up by the trimmer or the
replay path as ordinary history.

**It is never replayed into the model's context by default.** Reasoning is worth roughly its own
token count and is the first thing a human skips when rereading; replaying it would consume the
working set PAI-3 just fought to enlarge. This is the explicit interaction with
[PAI-4](./04-smart-compaction.md) and is stated here so nobody wires it "for continuity".

### 3.5 Fix `/agent/chat/stream` parity

The route needs thinking capture, the idle timeout, context monitoring, `turn_stats`, telemetry and
`persist_assistant_turn_with_extraction`. It also hand-parses `serde_json::Value` instead of using a
typed DTO (`routes.rs:7104-7117`), silently ignoring malformed `images`.

The right fix is to stop having two implementations: extract the shared stream body into one function
parameterised by `model_role` and the few genuine differences. Two copies of a streaming handler
diverging silently is how this gap appeared in the first place.

### 3.6 Resolve the prompt/engine inconsistency

One resolution function produces both the prompt-level section and the engine-level parameter, so
`off` means off in both. The name-based (not capability-cache-based) resolution stays exactly as it
is — its rationale at `goose_agent.rs:1000-1011` is a measured result, not a preference.

---

## 4. Phases

- **P1** `ChatEvent::Reasoning`; read `reasoning_content` in `pond-inference`; `GooseAdapter` emits
  `AgentStreamEvent::Thinking`. `ThoughtFilter` demoted to fallback.
- **P2** `UsageStats.reasoning_tokens` + migration; surfaced in `turn_stats` and `/usage/summary`.
- **P3 — ALREADY LANDED**, before this programme began. One `thinking_enabled` drives both the
  prompt section and the engine param; see section 1.7. Nothing to do.
- **P4** `reasoning_effort` tri-state mapped through the compaction profile; `brief` default on
  local.
- **P5** `output_reserve_tokens` derived from measured reasoning behaviour rather than a constant
  (needs P2's data).
- **P6** `persist_thinking` side table; UI disclosure in the existing thinking panel; never replayed.
- **P7** Unify the two stream handlers; `/agent/chat/stream` gains full parity, including memory
  extraction.

---

## 5. Invariants

1. Thinking resolution is name-based, not capability-cache-based, and the resulting prompt delta must
   not move the KV prefix between turns of the same session.
2. Voice mode never renders reasoning. It is unspeakable text.
3. Reasoning is never replayed into context by default.
4. `output_reserve_tokens` is never zero, whatever the measurement says.
5. `ThoughtFilter` stays. Structured channels are an optimisation, not a replacement — most local
   models still inline their tags.

---

## 6. Deliberate deferrals

- **Interleaved thinking between tool calls.** Depends on the fork's reply loop; revisit with PAI-6.
- **Reasoning-aware answer review.** `review_mode` exists (`settings.rs:386-400`) and could grade the
  reasoning rather than the answer. Real value, separate change, and it doubles inference cost.
- **Excluding reasoning from `agent_max_turns`.** This is B5 from the context roadmap, still a
  fork-side change in `goose/crates/goose/src/agents/agent.rs`.

---

## 7. Verification

- **Unit** — `ChatEvent::Reasoning` routing; the resolution matrix (mode × capability × voice)
  asserting prompt and engine always agree.
- **Canary** — a test asserting `ThoughtFilter`'s tag table still matches what the pinned models emit,
  in the spirit of `goose_cap_message_is_still_verbatim`.
- **Measured, Orin** — the documented overrun scenario: `n_ctx 4096`, a question that provokes long
  reasoning. Before: `ContextLengthExceeded` and a truncated answer. After: reasoning stops at
  budget and a complete answer arrives.
- **Integration** — `show_thinking` on `/agent/chat/stream` produces `thinking` events (it produces
  none today).
- **Regression** — reasoning tokens appear in `turn_stats` and are *not* double-counted in
  `completion_tokens`.
- **Manual** — `persist_thinking` on, reopen the session, confirm reasoning is visible in the UI and
  absent from the model's replayed context.
