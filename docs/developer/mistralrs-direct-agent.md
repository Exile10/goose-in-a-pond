# The direct mistral.rs backend — a checkpoint

`agent_backend = "mistralrs"` serves a turn **without goose**. It is the other
end of the experiment [`mistralrs-provider.md`](mistralrs-provider.md) started:
that one measured mistral.rs *through* the goose harness, this one measures it
with nothing in between.

Status: a checkpoint, kept behind an off-by-default cargo feature. It may be
reverted, or grown into the production path. Nothing depends on it.

## The two paths, side by side

| | `MODE=goose` | `MODE=direct` |
|---|---|---|
| backend | `GooseAdapter` | `MistralRsAgent` |
| loop | `goose::Agent::reply` | 50 lines in `agent.rs` |
| prompt | pond-core, re-imposed by `GiapProviderShim` after vetoing goose's | pond-core, sent as written |
| tools | goose extension manager over rmcp | `McpToolDispatcher` (also goose-free) |
| history | goose `sessions.db` | GIAP `SessionStorage` |
| transport | goose OpenAI provider | `MistralRsProvider` |

Same server, same model, same system prompt. What differs is what sits between
them, which is the thing being measured.

```bash
~/Documents/Jarida/mistralrs-bakeoff/run.sh     # mistral.rs on :9002
scripts/try-mistralrs.sh                        # direct  (default)
MODE=goose scripts/try-mistralrs.sh             # through goose
```

Build: `cargo build --release -p pond-server --features mistralrs-agent`.
The script refuses a binary without the feature rather than silently falling
back — a stale binary is correct source, old behaviour, and no error anywhere.

## The boundary, and how it is kept

`crates/pond-adapters-mistralrs` depends on **`pond-core` and nothing else** in
this workspace. Tools arrive through the `ToolDispatcher` port, history through
`SessionStorage`, and the system prompt is built by `pond_core::prompts` +
`models::services::prompt_builder`.

That is the property the checkpoint exists to test, and it is enforced by one
line in a `Cargo.toml`. **A `goose` dependency added there would end the
experiment without failing a single test** — treat one as a review failure.

## What was carried over, and why only that

The system prompt. It is not goose's: the v2 tag skeleton, the compact/full
tiering, the static/dynamic partition that keeps a KV prefix token-stable, the
extras and skills envelopes — all of it is GIAP's own code in `pond-core`, and
it is the accumulated result of the prompt work. Rewriting it would have thrown
away the part worth keeping and made the comparison meaningless at the same
time.

One adaptation: `native_tools_json = true` and an empty `available_tools`.
Tools travel structurally in the request body, so the template must not also
render a list of them; feeding both is how a model ends up describing tools
instead of calling them.

## What it deliberately does not do

No answer review, no delegation, no memory extraction, no hybrid compaction, no
vision, no recipes, no prefix prewarm, no MTP. Those live on the goose path and
are not reimplemented here. A turn that needs them belongs on that path.

This matters when reading a comparison: the direct path is not doing the same
amount of work, and some of what it skips costs real time on the goose path
(memory extraction runs a second inference). A TTFT difference is a fair
comparison; a wall-clock-per-turn difference is not, unless the goose side has
those features off too.

## Measured, 2026-09-10 — both paths, one server, one session

Same Mac, same debug `pond-server`, same scratch pond, same mistral.rs on :9002
(`gemma-4-E2B-it-qat-UD-Q4_K_XL`, `--paged-attn on`, ctx 8192,
`--prefix-cache-n 0`), same three prompts. Only `MODE` differed.

| prompt | path | TTFT | prompt tok | inferences | outcome |
|---|---|---:|---:|---:|---|
| "remember … teal" | direct | 6.5 s | 5,576 | 1 | **tool call leaked as text** |
| | goose | 8.8 s | 7,400 | 3 | tool called; reasoning leaked into the answer |
| "what is my favourite colour?" | direct | 5.8 s | 5,623 | 1 | correct, from history |
| | goose | 8.6 s | 7,522 | 2 | correct, from the saved memory |
| "what time is it?" | direct | 6.5 s | 5,706 | 2 | tool called, correct answer |
| | goose | 8.9 s | 7,633 | 2 | tool called, correct answer |

**TTFT and `prompt_tokens` are comparable; decode is not.** The two paths derive
a decode rate by different rules, so the figures are not the same quantity and
are left out of the table on purpose.

Read the TTFT column with the prompt column beside it. The direct path is
~2.3 s faster per turn, and it is also sending ~1,900 fewer tokens — a shorter
preamble and a smaller tool list, not purely less overhead. Some of the harness
cost is prompt, which is a real cost, but it is not the same claim as "the
harness is slow".

**The direct path is faster and the goose path is more reliable**, on this
evidence. Two of six turns on the direct path went wrong in ways the goose path
did not: one emitted `giap-memory__save_memory{content:<|"|>…}` as literal text
instead of a structured `tool_calls` delta, and one separate probe ("list the
devices in my home") returned a single completion token and no content at all.
Both have known causes and neither is fixed here:

- **Text-form tool calls.** Gemma 4 can emit `<|tool_call>` markup instead of
  the structured protocol, which is exactly what `Agent::call_tool` exists to
  catch on the goose path. This agent has no such fallback, so the markup
  reaches the user as the answer. The same server parsed
  `giap-system__get_current_time` structurally minutes earlier, so this is
  mistral.rs's gemma4 parsing being inconsistent, not a model that cannot do it.
- **Empty turns.** gemma-4-E2B reliably produces nothing for certain phrasings;
  the goose path re-engages with `EMPTY_TURN_STEER` and counts it in
  `reengagements`. This agent does not re-engage, so an empty turn is simply
  empty.

**Tools take the whole history budget.** 46 tools is 24,192 characters of
schema against an 8,192-token window, so `available_history_chars` saturates to
its floor and the history that survives is roughly the current turn. Follow-ups
still work here because `giap-memory` saved the fact — not because the
conversation was in the prompt. The goose path applies per-turn tool selection
(`tool_selection_mode`); this agent asks the dispatcher for everything, every
turn, which is the largest single gap between them.

## The numbers it reports, and their provenance

mistral.rs's OpenAI surface publishes no prefill timing, so:

| `TurnStats` field | source |
|---|---|
| `ttft_ms` | **wall clock**, POST to first delta — includes queueing and network |
| `decode_ms` | wall clock, first delta to end of stream |
| `prefill_ms` | **`None`** — deliberately not filled with a number of a different provenance |
| `prompt_tokens` / `completion_tokens` | the server's own `usage` frame |

`prefill_tok_per_sec` is therefore always `None` here. Filling it from wall
clock would produce a number that gets compared against llama.cpp's
engine-reported one, which is a different quantity.

`decode_ms` is only recorded when the window is at least 100 ms. A round that
ends in a tool call emits its whole `tool_calls` delta at once, so the window is
near zero and the quotient came out at **230,000 tok/s** in the bake-off
harness. A number that cannot be true is worse than no number.

## Three things the wire protocol gets wrong if you read the spec

1. **Thinking is in `delta.reasoning_content`, not `delta.content`.** A parser
   that reads only `content` records a turn that produced nothing. The first
   run of the bake-off harness reported 0 tool calls and 0 tok/s across every
   engine for this reason alone.
2. **A failed stream returns HTTP 200 with `{"error":{…}}` in the SSE body.**
   mistral.rs's own `/metrics` counts it as a healthy request. `wire.rs` parses
   that field, and `provider.rs` turns it into a real error, which is the only
   reason the failure is visible at all.
3. **`/v1/models` must be asked.** mistral.rs answers a model name it does not
   serve with a 400 rather than defaulting to the single model it loaded, and
   GIAP's model id carries a quant tag the server never saw. The provider
   resolves the served id once, lazily, and falls back to the configured name
   so an unreachable server fails at the chat call with a real message.

## Known limits of the checkpoint

- **No per-turn tool selection.** Every turn carries every tool. See the
  measurement above for what that costs.
- **No fallback for text-form tool calls**, and **no empty-turn re-engagement**.
  Both are measured above.
- **`serve` only.** `pond-server chat` still goes through `build_goose_backend`.
- **The agent is built from settings read at startup**, so `chat_model` and the
  context window are whatever was persisted at boot. The script's two-phase
  start is what makes that correct; a PUT into a running server cannot reach a
  backend that is already wired.
- **`prefix_cache_state` and `prewarm` are the port defaults** — no prefix is
  tracked, so PAI-4 P5 sees this agent as permanently warm. That is the correct
  narrowing answer for an agent with no visible cache, but it means the
  trimmer's posture carries no information here.
- **mistral.rs's own prefix-cache bug still applies.** With tools in the
  payload it corrupts on the second request; `run.sh` passes
  `--prefix-cache-n 0`. See [`mistralrs-provider.md`](mistralrs-provider.md) —
  disabling it is what makes TTFT 7.5–7.9 s on a real GIAP prompt.

## Reversing it

```
crates/pond-adapters-mistralrs/          delete
Cargo.toml                               one member line
crates/pond-server/Cargo.toml            one feature, one optional dep
crates/pond-server/src/main.rs           the mistralrs_active flag and its branch
scripts/try-mistralrs.sh                 MODE=direct
```

Nothing in `pond-core`, `pond-api` or `pond-infra` was touched, so a revert
cannot leave a port or a settings row behind.
