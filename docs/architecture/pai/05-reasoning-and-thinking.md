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

### 1.2 `AgentStreamEvent::Thinking` had zero producers

> **CORRECTED 2026-08-06 by P1.** The observation ("no adapter ever emits it") was right; the
> diagnosis under it was wrong, and the wrong diagnosis is what sent P1 at the wrong crate. See the
> P1 stamp in section 4. The line numbers below are re-verified as of 2026-08-06 — every one of the
> four originally recorded here had rotted.

Four references exist in `crates/`, all consumers:

| Site | What it does |
|---|---|
| `shared/services/chat.rs:1101` | matches it and **ignores** it |
| `pond-api/src/routes.rs:1332` | forwards as a `thinking` SSE event |
| `pond-api/src/routes.rs:7919` | same, on `/agent/chat/stream` |
| `pond-server/src/main.rs:6388` | CLI, prints dimmed to stderr |

No adapter emitted it **until P1**. The reason was not that nothing produced reasoning — Goose's own
`goose-local-inference` has always read llama.cpp's `reasoning_content` and returned
`Message::assistant().with_thinking(..)`, and `goose-provider-types`' shared OpenAI format does the
same for every HTTP provider that separates the channel. `GooseAdapter` received those messages and
dropped the reasoning one call short of the seam, because `as_concat_text()` filters on `as_text()`,
which returns `None` for `Thinking`.

Before P1, thinking reached the UI **only** through `ThoughtFilter`, a streaming state machine
(`pond-api/src/thought_filter.rs`) that scrapes tag-delimited blocks out of the text stream:
`<|channel>thought…<channel|>`, `<think>…</think>`, `<thought>…</thought>`, plus standalone
sentinels. It is constructed with capture only when `show_thinking && !voice_mode` (grep
`with_thinking_capture` in `routes.rs`).

The claim that "the entire feature rests on regex-matching whatever envelope the model happens to
emit" was true in general and **false for the local/gguf path with thinking ON**, which is the
Jetson's shipped configuration: Goose strips `<think>` from the text before GIAP sees it, so
`ThoughtFilter`'s capture path received nothing there and `show_thinking` was already non-functional.
P1 is what makes that path work at all.

### 1.3 The structured reasoning channel is parsed and thrown away

> **CORRECTED 2026-08-06 by P1.** This section pointed at the wrong crate. `pond-inference` feeds
> the **quarantined** PondAgent loop (Q2-05: `agent_backend = "pond"` is rejected by the API with
> 422 and overridden to `"goose"` at startup), so nothing described below has ever executed in a
> shipped configuration. The live throw-away was in `GooseAdapter`, and P1 fixed that one.

`pond-inference/src/provider.rs` documents that `ChatParseStateOaicompat` "extracts structured
deltas (content, reasoning_content, tool_calls)", and its delta-consumption loop reads
`delta["content"]` and `delta["tool_calls"]` only. That is still true and still unfixed — it is
simply not on the serving path, so it is carved out of P1 and left for whichever milestone
un-quarantines PondAgent.

`ChatEvent` (`models/ports/inference.rs`) is `Text | ToolCall | Usage`. There is no reasoning variant
at the port level. This is **irrelevant to the live path**, which streams `AgentStreamEvent` — a type
that has carried a `Thinking` variant all along.

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

> **SUPERSEDED IN PART 2026-08-06 by P1.** The `ChatEvent` half below applies only to the quarantined
> PondAgent loop and was not implemented. The live path needed no new port type: `AgentStreamEvent`
> already had `Thinking`, and the work was to stop discarding the messages that carry it. The
> `ThoughtFilter` demotion in the last paragraph was also not implemented — see the P1 stamp for why
> the double-reporting it guards against has no observed path.

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

> **CORRECTED 2026-08-06 by P2.** Two claims below were wrong. The migration number was stale —
> the sequence ends at `0038_drafts_owner.sql`, so P2 took **0039**. And the section read as though
> the count arrives from the provider: it does not, for any provider GIAP ships. Goose's `Usage`
> has input/output/total/cache_read/cache_write and no reasoning field, and the OpenAI-shaped
> providers separate reasoning into a *content* channel rather than a counter. The number is
> GIAP's own, counted from the text P1 captured through the PAI-3 `TokenCounter` port. See the P2
> stamp in section 4.

`UsageStats` gains `reasoning_tokens: Option<u32>`, persisted alongside the existing per-message
counts (migration `0039`, extending `0029`'s columns). This closes the loop with PAI-3: the governor
can then set `output_reserve_tokens` from **measured** reasoning behaviour for the active model
rather than from a constant chosen to survive the worst case observed once on an Orin.

The cost model also becomes honest — though "currently attributes reasoning to completion" was an
assumption, not a finding. Whether the provider's `output_tokens` already covers the reasoning
decode is **unmeasured for every model GIAP pins**, which is exactly why P2 reports the two numbers
side by side and subtracts nothing.

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

- **P1 — LANDED 2026-08-06, respecified onto the live path, and two of its three clauses were
  deliberately not done.** `crates/pond-adapters-goose/src/goose_agent.rs`:
  `GooseAdapter::reasoning_frames(&Message, emit) -> Vec<String>` lifts `MessageContent::Thinking`
  out of every `AgentEvent::Message` and the stream yields it as `AgentStreamEvent::Thinking`,
  before that message's tool calls and answer text — the order the provider produced them in.
  `GooseAdapter::reasoning_frames_enabled(show_thinking, voice)` is the gate. Five unit tests;
  105 pass in the crate.

  > **CORRECTED 2026-08-06 (synthesis): the gate was tested; its INPUT was not, and the input is
  > the whole defence on the shipped desktop.** `reasoning_frames_enabled` had all four rows of its
  > truth table asserted, and the source guard pinned the literal
  > `Self::reasoning_frames_enabled(settings.show_thinking, is_voice)` — but that pins the
  > *identifier* `is_voice`, never its meaning. A review changed the binding from
  > `voice_instance || request.voice_mode` to `voice_instance` and **all 108 tests passed**. That
  > mutation makes reasoning leak to every voice turn with `show_thinking = true`, silently, and it
  > is not theoretical: `main.rs` builds the serve-mode adapter with `voice_mode: false` hardcoded,
  > so the instance flag is never true in the desktop/server process and `request.voice_mode` —
  > sent by `WebVoiceBackend.ts` — is the only signal that ever goes true. `routes.rs` forwards
  > `AgentStreamEvent::Thinking` unconditionally on both handlers, so the producer gate really is
  > the only defence, exactly as this phase argued, and its input was unguarded.
  >
  > This is the `ProfileScope::Owner` shape one level up: the fixture is reachable in production,
  > but no test held it reachable. Fixed by extracting the policy into a testable unit —
  > `GooseAdapter::voice_turn(instance, request)` — called at the binding site, with all four rows
  > asserted by `a_request_flagged_voice_is_a_voice_turn_even_on_a_text_started_process`. The
  > `(false, true)` row is asserted first and by name, because it is the shipped-desktop case. The
  > source guard additionally pins the composition, not just the token. Both mutations now go red.

  > **RESOLVED 2026-08-06 by P1-granularity — see the stamp directly below this note.** The
  > diagnosis under it is correct and is left standing verbatim, because it is the record of what
  > the defect was and it is the thing that falsifies the fix.
  >
  > **OPEN 2026-08-06 (synthesis), NOT fixed: the frame granularity is wrong, and the phase's claim
  > that "the consumer already exists, so this is observable end-to-end with no further work" is
  > false in shape rather than in degree.** P1 emits ONE `Thinking` frame per `AgentEvent::Message`.
  > On its own headline reachable configuration — local/gguf on the Jetson — goose emits one
  > `AgentEvent::Message` **per reasoning delta**, i.e. per token piece:
  > `goose-local-inference/src/llamacpp/inference_native_tools.rs` calls
  > `push_structured_reasoning` from inside the per-token `|piece|` callback and it returns the raw
  > unbuffered delta; `goose-provider-types/src/formats/openai.rs` yields only the newly-accumulated
  > slice. Nothing upstream coalesces them — `reply_parts.rs`'s
  > `should_suppress_replayed_thinking` requires `has_tool_requests`, so a pure-reasoning chunk is
  > never suppressed.
  >
  > The consumer treats every frame as a complete, discrete reasoning block:
  > `sections/Chat.tsx` does `thinkingBlocks: [...(last.thinkingBlocks ?? []), ev.content]` and
  > renders each as its own `<p>`. So one reasoning passage renders as hundreds of one-fragment
  > paragraphs, with the inter-fragment whitespace destroyed by `reasoning_frames`' per-fragment
  > `.trim()`. Note the asymmetry P1 stepped into: `Chat.tsx` CONCATENATES text deltas
  > (`last.text + visible`) and APPENDS thinking frames. The contrast that establishes the intended
  > contract is `thought_filter.rs`, which captures the COMPLETE body between paired tags.
  >
  > Not fixed here because it is a streaming-semantics change on the live path and the coordinator's
  > live run has not happened yet — this is precisely a "does it render twice / does it render as
  > confetti" question that a unit test answers less well than one turn on a real Jetson. Every P1
  > test is single-message, which is why the suite is green. **The fix belongs in the adapter**
  > (`Chat.tsx` renders it and the consumer's append is reasonable): accumulate reasoning per
  > message and emit one frame per completed block, or keep per-delta frames and stop trimming.
  > Either way the test must drive a SEQUENCE: `[" the user", " asked about", " the light"]` must
  > round-trip to `"the user asked about the light"`, not `"the userasked aboutthe light"`.

  > **P1-granularity — LANDED 2026-08-06. Reasoning frames are coalesced per block in
  > `GooseAdapter`, flushed on visible output and at end of stream.** One file:
  > `crates/pond-adapters-goose/src/goose_agent.rs`. No route, no handler, no migration, no startup
  > wiring, no `Settings` field — `routes.rs`, `main.rs` and `Chat.tsx` keep working unchanged,
  > which is the point of fixing this at the producer.
  >
  > **What landed.** `ReasoningCoalescer { buf: String }` with `push(&Message, emit)` /
  > `over_cap()` / `flush() -> Option<String>`. `reasoning_frames(msg, emit)` keeps the display gate
  > INSIDE it — that is the load-bearing P1 decision and moving it to the call site is how it gets
  > lost — but it is now the RAW fragment lift: no per-fragment `.trim()`, no dropping of
  > whitespace-only fragments, because a lone `" "` is the space between two words. Trim-once and
  > drop-if-blank moved into `flush()`, so P1's user-visible claim (no blank frame, no ciphertext)
  > is kept at the surface that actually produces a frame. `RedactedThinking` is still dropped in
  > the lift, so provider ciphertext never enters the buffer at all.
  >
  > **Why the flush condition is load-bearing rather than decoration.** The round-1 note reads as
  > though every provider emits deltas. It does not, and an unconditional turn-wide merge would be
  > WRONG:
  >
  > | provider family | one `AgentEvent::Message` is… | source |
  > |---|---|---|
  > | local / gguf (the Jetson headline config) | one **token piece** | `goose-local-inference/src/llamacpp/inference_native_tools.rs` calls `push_structured_reasoning` from inside the per-token `\|piece\|` callback; `thinking_output.rs` returns the raw delta |
  > | openai-format HTTP (Ollama, DeepSeek, OpenRouter, vLLM) | one streamed **delta** | `goose-provider-types/src/formats/openai.rs` pushes each chunk's newly-arrived `reasoning_text()` |
  > | google | one **part** | delta-shaped |
  > | anthropic | one **complete block** | `formats/anthropic.rs` accumulates `ThinkingDelta` in local state and emits once at `content_block_stop` |
  > | any non-streaming response | one **complete block** | `response_to_message` |
  >
  > So the flush condition is `message_ends_reasoning(msg)`: true iff the message carries something
  > **this adapter would yield** — a `ToolRequest`, a `ToolResponse`, or non-empty
  > `as_concat_text()`. Defined that way rather than "has text" because a `SystemNotification`-only
  > message produces nothing in GIAP, so splitting there would cut a passage in half invisibly.
  > `Thinking` and `RedactedThinking` never end a block. The consequence for a whole-block provider
  > is that its block is followed by text or a tool call and therefore flushes on its own, emitted
  > verbatim and unmerged; two of its blocks can only be adjacent across a tool round-trip, which
  > itself flushes. The consequence for a delta provider is that fragments accumulate across the
  > `Usage` and `HistoryReplaced` events that interleave them — different `AgentEvent` variants,
  > which never touch the buffer — and land as one passage.
  >
  > **The finding that kills the streaming-responsiveness objection.** `sections/Chat.tsx:633` gates
  > the entire thinking panel on `!msg.streaming`. Nothing is rendered until the turn ends, so
  > per-delta frames bought ZERO streaming responsiveness on the shipped desktop; what they bought
  > was hundreds of React state updates (`[...prev.slice(0,-1)]` per token) and hundreds of
  > one-fragment `<p>`s. There is no user-visible cost to buffering, and `pond-server/main.rs`
  > prints thinking with `\r\x1b[K` (overwrite), where per-delta frames were a flicker.
  >
  > **A cap, not a promise.** `REASONING_BUFFER_LIMIT = 64 KiB` force-flushes and logs a `WARN`. A
  > provider that never produces visible output would otherwise grow the buffer for a whole turn.
  > This is bounded memory, not a correctness rule: crossing it splits the passage rather than
  > dropping it.
  >
  > **What I deliberately did NOT do.** (a) I did not move `count_reasoning_tokens` into the
  > coalescer — see the new open item below; the coalescer is display-gated and moving the count
  > into it is exactly the `Some(0)`-for-the-whole-corpus regression synthesis closed one note up.
  > (b) I did not touch `Chat.tsx`. Its append is correct once the producer emits blocks, and the
  > file is held by another workstream this round. (c) I did not change persistence: P6 still owns
  > that, and reasoning still never reaches `session_messages`. (d) I did not add a `Settings`
  > field; the granularity is not a user choice.
  >
  > **Guards, and the three mutations run against them.** New:
  > `a_reasoning_passage_arrives_as_one_frame_with_its_spacing_intact` (the SEQUENCE the note above
  > demands, asserting the frame COUNT is 1 as well as the text),
  > `a_whole_block_provider_is_not_merged_into_one_giant_block`,
  > `a_block_that_ends_the_turn_is_not_dropped`, `the_display_gate_still_owns_the_buffer`,
  > `the_last_reasoning_block_of_a_turn_is_flushed_after_the_event_loop`. Re-aimed:
  > `ciphertext_and_blank_reasoning_never_reach_the_stream` now asserts at the FLUSH (asserting the
  > raw lift is non-empty would have been the guard degrading into a restatement of the change).
  > Mutations, each restored byte-identical: **M1** put `.trim()` back on each fragment in the lift —
  > test 1 failed printing `"the userasked aboutthe light"`, naming destroyed whitespace rather than
  > a bare assert. **M2** deleted the post-loop flush — exactly ONE guard caught it,
  > `the_last_reasoning_block_of_a_turn_is_flushed_after_the_event_loop`, printing the two line
  > numbers it looked between and naming the dropped final passage. Worth recording that
  > `a_block_that_ends_the_turn_is_not_dropped` stayed GREEN under M2 and recon predicted it would
  > fail: it is a pure coalescer test and knows nothing about the stream's wiring, which is the
  > whole reason the positional guard has to exist alongside it. **M3** made
  > `message_ends_reasoning` return `false` always — test 2 failed showing the two blocks fused into
  > one frame.
  >
  > **Two source guards were rewritten, carefully, because this is where the fix could quietly
  > weaken what synthesis just repaired.** `every_thinking_frame_leaves_through_the_gate` asserted
  > `matches("yield Ok(AgentStreamEvent::Thinking").count() == 1`; there are two flush sites now,
  > and bumping that to `== 2` is precisely the "vacuity control that pins the wrong number" shape —
  > it would certify a third, ungated yield. It now iterates EVERY such yield and requires
  > `reasoning.flush()` in the three lines above it, plus exactly one
  > `reasoning.push(&msg, emit_reasoning)`, and it reads comment-stripped source so prose cannot
  > satisfy it. The `reasoning_frames_enabled(settings.show_thinking, is_voice)` and
  > `voice_turn(voice_instance, request.voice_mode)` composition pins are kept verbatim.
  > `the_reasoning_count_is_taken_outside_the_display_gate` `.expect()`ed the
  > `for content in Self::reasoning_frames(&msg, emit_reasoning)` line this phase deleted — it would
  > have PANICKED rather than reported. Re-anchored on `reasoning.push(&msg, emit_reasoning)`, with
  > the whole-eight-line-window scan kept.
  >
  > **The positional guard is anchored on the loop's closing brace, not on "after the `while let`
  > line".** Recon proposed the latter; it is vacuous, because the in-loop flush also sits after
  > that line, so deleting the trailing flush would have left it green — the exact failure it exists
  > to catch. It now locates the `while let Some(event_result) = goose_stream.next().await {` line,
  > finds the `}` at matching indentation, and requires a `reasoning.flush()` between that brace and
  > `if produced_visible {`.
  >
  > **Reachable in production.** A Jetson on `chat_provider = local` / gguf with
  > `show_thinking = true` and `thinking_mode` on — the shipped headline configuration — takes this
  > path on every turn, and the frames reach `Chat.tsx` through `routes.rs` unchanged.
  >
  > **What would falsify it.** One turn on that configuration whose thinking panel still renders as
  > many one-word paragraphs; or a passage that renders with its inter-word spaces missing; or an
  > anthropic-family turn whose two reasoning blocks render as one; or a turn ending in pure
  > reasoning (gemma-4-E2B does this) that renders no thinking panel at all.
  >
  > `RUSTFLAGS="" SQLX_OFFLINE=true cargo test -p pond-adapters-goose` — 114 pass, 0 fail;
  > `cargo fmt --check` clean; `cargo clippy -p pond-adapters-goose --all-targets` emits nothing new
  > in `goose_agent.rs`.

  > **OPEN 2026-08-06 (P1-granularity), NOT fixed and NOT to be fixed inside the coalescer: PAI-5
  > P2's reasoning count is taken per MESSAGE, which on the delta providers means per token piece.**
  > `count_reasoning_tokens` is called once per `AgentEvent::Message`, so on local/gguf and on the
  > openai format it counts a ~4-character fragment at a time. With `HeuristicTokenCounter`
  > (`token_counting.rs`, `text.len() / CHARS_PER_TOKEN`, integer division, no `max(1)`) that
  > truncates to **0** for most fragments — a large systematic UNDERCOUNT of the number P5 sizes
  > `output_reserve_tokens` from. It only bites when tiktoken fails to build and
  > `token_counter_handle()` falls back; with tiktoken it is roughly right.
  >
  > **Do not "fix" this by counting at flush time.** The coalescer is display-gated by design, and
  > `chat.rs::stream_response_inner` — the only path that persists the number — builds its request
  > with `voice_mode: true`, so `emit_reasoning` is always false there. Moving the count into the
  > coalescer would write `Some(0)` for 100% of the corpus, which is the exact regression the
  > correction two notes above closed. A real fix needs a second, UNGATED accumulator for the raw
  > fragment text, which is more machinery than that phase warrants; it belongs wherever P5's
  > reserve is next touched.

  **The phase as written targeted a crate that cannot execute.** `ChatEvent::Reasoning` and
  `pond-inference`'s `reasoning_content` both belong to the quarantined PondAgent loop (Q2-05), so
  implementing them literally would have changed nothing a user can observe — a third "correct but
  unreachable" mechanism in a programme that already has two. Sections 1.2 and 1.3 carry the
  correction. **Neither was implemented and neither should be until PondAgent is un-quarantined**;
  if that milestone ever arrives, the port-level variant is a five-line change and this is the note
  that says so.

  **The producer already existed; the drop was ours.** `goose-local-inference` reads llama.cpp's
  `reasoning_content` and returns `Message::assistant().with_thinking(..)` from three call sites in
  `llamacpp/inference_native_tools.rs` (and `mlx.rs`, and the emulated-tools path);
  `goose-provider-types`' shared OpenAI format does the same for DeepSeek/OpenRouter/vLLM-shaped
  responses, which is the Ollama path. All of it arrived in `AgentEvent::Message` and died on
  `as_concat_text()`, which filters on `as_text()` — `None` for `Thinking`. A test asserts that
  `as_concat_text` is still the answer-only view, because that is the whole reason a separate lift
  has to exist.

  **The gate is at the PRODUCER, and that is the load-bearing decision.** Recon's plan was to gate
  the two SSE forwards in `routes.rs`. That file was held by a concurrent group, but it was also the
  wrong place: there are **three** consumers of this event, not two — `routes.rs` twice and
  `pond-server`'s CLI printer, which is the terminal voice loop, writing dimmed to stderr
  unconditionally. Exactly one of the three had ever consulted `show_thinking`. Gating at the seam
  would have required editing all three and left the fourth consumer to be discovered later. One
  gate at the producer is inherited by every consumer, present and future, and it is the contract
  `AgentStreamEvent::Thinking`'s own doc comment already claimed. **Consequence: `routes.rs` needed
  no change at all**, which is also why this landed without touching a held file.

  `voice` is `voice_instance || request.voice_mode` — the CLI `--input whisper` flag OR the
  per-request desktop flag, unlike `vision_section_applies` which reads only the instance flag. It
  can be the OR here precisely because this value never reaches `PromptState`: it is resolved after
  the prompt is built, so it cannot move the static prefix between turn 1 and turn 2. No prompt text
  changed; the KV prefix is untouched.

  **`ThoughtFilter` was NOT demoted, and the demotion should not be added blind.** The plan asked
  for its capture path to be suppressed once a structured frame is seen, to prevent double-reporting.
  No path was found that produces both: on local/gguf, Goose strips `<think>` from the text before
  GIAP sees it, so the scraper receives nothing to double; where the scraper does fire (a model
  inlining tags with `enable_thinking` off) there is no structured block. Building a cross-crate
  suppression signal for a collision nobody has observed would have meant editing a held file to
  guard against a hypothesis. `ThoughtFilter` therefore still runs unconditionally for text hygiene,
  which it is needed for regardless — GIAP's `<|channel>thought` and `<thought>` envelopes are not in
  Goose's own `ThinkFilter` at all.

  **What would falsify this.** A turn on `chat_provider = local`/`gguf` with `show_thinking = true`
  and `thinking_mode` on that renders no thinking panel in `Chat.tsx` — the frontend already handles
  `ev.type === "thinking"` and the `ChatEventType` union already lists it, so no UI change was
  needed and no UI change can be the excuse. Equally falsifying: the same panel appearing twice for
  one block, which would mean the `ThoughtFilter` collision above is real after all.

  **Not persisted, on purpose.** P6 owns that. Until it lands, reasoning is streamed and forgotten:
  `chat.rs` matches `Thinking` and ignores it, so nothing enters `session_messages` and nothing is
  replayed into context.
- **P2 — LANDED 2026-08-06 as the producer and the store. The two display surfaces it named are
  NOT done, and the reason is a held file, not a decision.**

  `UsageStats.reasoning_tokens: Option<u32>` (`models/ports/provider.rs`),
  `TurnStats.reasoning_tokens: Option<u32>` (`shared/domain/turn_stats.rs`),
  `SessionMessage.reasoning_tokens` (`user_data/domain/session.rs`) with
  `with_reasoning_tokens`, and migration `0039_reasoning_tokens.sql` —
  `ALTER TABLE session_messages ADD COLUMN reasoning_tokens INTEGER`, nullable, **no DEFAULT**.
  `GooseAdapter::count_reasoning_tokens(&Message, &dyn TokenCounter)` accumulates into `turn_stats`
  in the stream loop and both arms of the `usage` build carry it out.

  **The number is GIAP's, not the engine's, and the plan said otherwise.** Section 3.2 implied
  `reasoning_tokens` arrives from the provider. It does not and will not: Goose's `Usage`
  (`goose-provider-types/src/conversation/token_usage.rs`) carries input/output/total/cache_read/
  cache_write and no reasoning field, and the providers that *do* separate reasoning put it in a
  content channel, not a counter. So this counts the thinking text P1 captured, through the PAI-3
  P2 `TokenCounter` port — tiktoken when it is built, chars/4 otherwise, and `is_exact()` is false
  for both. The doc comments say so at every one of the three declarations, because a number whose
  provenance is only in a commit message gets read as ground truth within a release.

  **It is not subtracted from `completion_tokens`, and that is a decision, not an oversight.** The
  provider's output count most likely already includes the reasoning decode — but nobody has
  measured which way for the models GIAP pins, and subtracting a GIAP estimate from an engine-
  reported number corrupts the one that was actually measured. `turn_stats.rs ::
  reasoning_does_not_move_the_completion_count_or_the_decode_rate` holds that line, and
  `finalize_rates` deliberately ignores the field so `decode_tok_per_sec` stays a rate over what the
  engine reported.

  **Counted ungated, displayed gated.** `count_reasoning_tokens` does not consult `show_thinking`;
  only `reasoning_frames` does. The model spends the tokens either way, and the shipped default is
  `show_thinking = false`, so a count that moved with the checkbox would report every Jetson turn as
  having done no thinking — and P5 sizes `output_reserve_tokens` from exactly this number.
  `the_reasoning_count_is_taken_outside_the_display_gate` asserts structurally that the accumulation
  sits above the frame loop; mutation-tested by moving it inside, which fails naming that
  consequence.

  > **CORRECTED 2026-08-06 (synthesis): that guard was vacuous against the regression it names, and
  > the phase's mutation happened to pick the one syntactic form it catches.** Moving the statement
  > INSIDE the `for` loop fails, as reported. Wrapping it in `if emit_reasoning { … }` on the
  > preceding line — the way anyone would actually couple the count to the display gate — left all
  > seven reasoning tests green, `the_reasoning_count_is_taken_outside_the_display_gate` included.
  > The guard located the single line holding `Self::count_reasoning_tokens(&msg,` and checked only
  > THAT line for `emit_reasoning`; a conditional one line up is invisible to it. A `let gated = …`
  > computed above it also passed.
  >
  > The consequence is worse than a missed regression. `chat.rs::stream_response_inner` builds its
  > `AgentRequest` with `voice_mode: true` unconditionally, and that is the **only** path that
  > persists this number — so on `run_chat`, `emit_reasoning` is always false. A gated count would
  > have written `Some(0)` for 100% of the corpus P5 reads, while every test that exercises the
  > pure counter kept passing. Fixed: the guard now scans the whole eight-line window and fails on
  > any `emit_reasoning` in it, quoting the offending line.
  >
  > **A second dark link, same shape.** Both `UsageStats` construction arms were replaced with a
  > literal `reasoning_tokens: None` and all 108 tests passed — this phase's own named failure mode
  > ("produced, reaches `Done`, and is dropped") landing silently on the path where it is supposed
  > to survive. The guard now asserts both arms read `turn_stats.reasoning_tokens`. There are two
  > because the usage build has a reported-usage path and a fallback path, and a regression fixing
  > only one is worse than one fixing neither, because it then depends on the provider.
  >
  > **Still dark, and NOT fixed here** — `ChatService::persist_assistant_response`'s
  > `.with_reasoning_tokens(usage.and_then(|u| u.reasoning_tokens))` can be replaced with `None`
  > and all 804 `pond-core` tests pass. The adjacent link IS guarded (mutating
  > `SessionMessage::with_reasoning_tokens` to a no-op fails the `pond-infra` round-trip), so the
  > store is proven to round-trip a hand-built row; nothing connects the counted number to the
  > column. It needs one test against a fake `SessionStorage` asserting the captured
  > `SessionMessage.reasoning_tokens == Some(N)`. Left for P6, which is already opening this file.

  **`None` is not `Some(0)`.** "Nobody counted" and "counted, and this turn thought nothing" are
  different facts and P5 must not read the first as the second. That is why 0039 has no `DEFAULT 0`:
  a default would retroactively assert of every pre-existing row that its turn did no thinking.
  `reasoning_tokens_round_trip_beside_the_provider_counts` reaches that case with a raw INSERT that
  omits the column — the adapter always binds it, so binding NULL through the adapter would pass
  against `DEFAULT 0` just as happily. Mutation-tested by adding the default.

  **What was deliberately NOT done, and why.** `crates/pond-api/src/routes.rs` was concurrently held
  by the PAI-2 security work, so the `turn_stats` SSE frame and `GET /usage/summary` do not carry
  the field. Both are in that file: the frame is a hand-built `json!` (adding a field to `TurnStats`
  does not flow into it), and `/chat/stream` flattens usage into two `u32` locals and hands
  `ChatService` a `(prompt, completion)` **tuple**, which cannot express a third number. So on the
  desktop route the count is produced, reaches `AgentStreamEvent::Done`, and is dropped; the
  persisted column is NULL for those rows, which is the honest value. It is carried end to end on
  the `ChatService` path — `pond chat` and the terminal voice loop — where `turn_usage` is a whole
  `UsageStats`: it is persisted by `persist_assistant_response` and printed as `reasoning N tok` in
  the stdout turn summary. Widening `persist_assistant_turn`'s tuple was considered and rejected:
  every caller of it is in routes.rs.

  **Also not done:** `pond-inference`'s `reasoning_content` is still parsed and dropped (quarantined
  path, same carve-out as P1), and nothing counts reasoning for a non-Goose provider — those
  `UsageStats` sites pass `None` explicitly rather than a flattering zero.

  **What would falsify this.** A turn on `local`/`gguf` with a thinking model whose stdout summary
  prints no `reasoning` part, or prints `reasoning 0 tok` while the thinking panel shows text — the
  first means the count never reached `TurnStats`, the second means it was taken from the gated
  lift. Equally falsifying: `session_messages.reasoning_tokens` coming back `0` for rows written
  before 0039, which would mean the column grew a default.
- **P3 — ALREADY LANDED**, before this programme began. One `thinking_enabled` drives both the
  prompt section and the engine param; see section 1.7. Nothing to do.
- **P4** `reasoning_effort` tri-state mapped through the compaction profile; `brief` default on
  local.
- **P5** `output_reserve_tokens` derived from measured reasoning behaviour rather than a constant
  (needs P2's data).
- **P6 — NOT LANDED 2026-08-06. Blocked, and the blocker is file ownership, not design.**
  `persist_thinking` side table; UI disclosure in the existing thinking panel; never replayed.

  P6 needs four seams and **all four are in files another workstream held for this run**:

  | Seam | File | Why it cannot move |
  |---|---|---|
  | The gate — `persist_thinking`, default `false` | `pond-core/src/user_data/domain/settings.rs` | held (security) |
  | The write — accumulate the thinking text and hand it to `ChatService` | `pond-api/src/routes.rs` (`chat_stream` ~1332/1560, `agent_chat_stream` ~7919/8020) | held (security) |
  | The read — rehydrate on `GET /sessions/{id}/messages` | `pond-api/src/routes.rs` (`get_session_messages`, registered at ~142) | held (security) |
  | The render — refill `thinkingBlocks` on session load | `pond-desktop/src/sections/Chat.tsx` | held (frontend) |

  **Nothing was landed instead, deliberately.** The unheld half — migration `0040`, a
  `pond-infra` store, a `pond-core` port — is buildable in an afternoon and would have been a
  **write-only table with no caller, no gate and no reader**. That is not a partial P6; it is a
  third "correct but unreachable" mechanism next to PAI-4 P2 and P7a, and this one would be
  strictly worse than the other two: it would accumulate reasoning text — which restates household
  context verbatim — on disk, with no setting to refuse it and no surface to view or delete it. A
  privacy store whose only property is that it exists is a defect, so it was not written.

  **There is no alternative write site, and that was checked rather than assumed.**
  `AgentStreamEvent::Thinking` has exactly four consumers in the workspace. Two are the held
  `routes.rs` handlers. The third is `main.rs:6452` (`pond agent`), outside this group's footprint
  and inside the startup-wiring file. The fourth is `chat.rs:1101` — the **voice** loop, where
  invariant 2 forbids rendering reasoning and P1's `reasoning_frames_enabled(show_thinking, voice)`
  gate means the frame never arrives at all. Persisting there would violate the invariant rather
  than satisfy the phase. Writing from `GooseAdapter` instead was rejected on two counts: it puts
  policy in an adapter, and `ChatService` is declared the sole owner of turn persistence.

  **Migration number, announced so the next run does not collide:** P2 took `0039`, so **P6 takes
  `0040`** (`0040_session_thinking.sql`). Nothing was created under that name — it is reserved by
  this note, not by a file.

  **Corrections to the plan of record, for whoever picks this up.** The recon plan said reasoning
  "vanishes on reload because nothing is persisted". Accurate, but it understates the work: the
  blocks never reach `ChatService` in the first place, because `routes.rs` forwards the frame
  straight to SSE and never accumulates it — so P6 is a write path to build, not a read path to
  add. The plan also asked for the side table to "inherit the parent row's profile scoping"; note
  that the parent is `session_messages`, which carries no `profile_id` of its own — the scope has
  to come through `sessions.profile_id` (added in `0003`, indexed in `0037`), and a fixture that
  sets it by hand reproduces exactly the `ProfileScope::Owner` no-op that went undetected for a
  whole phase.

  **One piece of good news, verified rather than hoped.** Invariant 3 is already enforced by the
  schema, not by convention: `session_messages.role` carries a `CHECK (role IN ('user',
  'assistant', 'system', 'tool'))` since `0022`. A future refactor cannot quietly turn reasoning
  into a replayed message role — it would have to rebuild the table to do it. The side-table design
  is therefore the cheap path as well as the correct one.

  **What would falsify this deferral.** `routes.rs`, `settings.rs` and `Chat.tsx` all being
  unheld — at which point P6 is ordinary work with no unknowns left in it.
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
- **Regression** — reasoning tokens are reported *alongside* `completion_tokens` and never deducted
  from it. Landed as `turn_stats.rs ::
  reasoning_does_not_move_the_completion_count_or_the_decode_rate`. The `turn_stats` **SSE frame**
  half of this is still outstanding: it is built by hand in the held `routes.rs`. See P2.
- **Manual** — `persist_thinking` on, reopen the session, confirm reasoning is visible in the UI and
  absent from the model's replayed context. **Not runnable as of 2026-08-06:** there is no
  `persist_thinking` setting and no store behind it. See P6.
