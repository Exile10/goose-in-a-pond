# Token Usage Tracking

Two systems in GIAP count tokens. They answer different questions, they are allowed to disagree,
and conflating them has been a recurring source of wrong claims in this repository.

| | **Accounting** | **Budgeting** |
|---|---|---|
| Question | what did that turn cost? | how much history fits in the next one? |
| When | after the stream drains | before the agent turn starts |
| Source | the provider's own `Usage` events, aggregated into `TurnStats` | the `TokenCounter` port |
| Accuracy | real when the provider reports; chars/4 only when it does not | **never exact** (section 2.2) |
| Consumers | `sessions` totals, `GET /usage/summary`, the savings card, `TurnMetrics` | `turn_trimmer`, `CompactionProfile` |

They meet in exactly one place, and it is the most load-bearing line here: the previous turn's
**accounting** number is fed back into the next turn's **budget** as the overshoot correction.
That feedback is what bounds the budget-side error, which is why it is not optional.

Verified against code 2026-08-06. PAI-3 P1 and P2 are landed; P3's catalog work is in flight and
P4-P5 are designed, so section 3's rung-3 row is the line here most likely to move next.

---

## 1. Accounting -- what the turn actually cost

### 1.1 Flow

```
  GooseAdapter::chat_stream
        |
        | aggregates every per-inference Usage event of the turn into
        | TurnStats  (a turn with tool round-trips has several inferences)
        v
  AgentStreamEvent::Done { usage: Some(UsageStats), stats: Some(TurnStats) }
        |
        +--> SSE done event: { usage: { prompt_tokens, completion_tokens } }
        |
        +--> session_storage.increment_usage(session_id, prompt, completion, model)
        |         -> sessions.total_prompt_tokens += N, total_completion_tokens += N
        |
        +--> session_messages.prompt_tokens / completion_tokens on the assistant row
        |
        +--> TurnMetrics.context_utilization_pct, via the telemetry sink in routes.rs
        |
        +--> last_prompt_tokens_handle()[session_id] = turn_stats.prompt_tokens
                  -> read back by trim_goose_history as `last_real` (section 2.3)
```

### 1.2 Real counts are the primary path

`GooseAdapter` accumulates the provider's per-inference `Usage` events into `TurnStats`
(`crates/pond-core/src/shared/domain/turn_stats.rs`) and reports those counts directly:

```rust
let usage = if saw_usage {
    turn_stats.context_used_tokens = Some(turn_stats.prompt_tokens);
    UsageStats { prompt_tokens:     turn_stats.prompt_tokens,
                 completion_tokens: turn_stats.completion_tokens }
} else { /* heuristic fallback, 1.3 */ };
```

`TurnStats.prompt_tokens` is the size of the **final** inference -- the turn's real context load --
while `completion_tokens` is summed across the turn. `TurnStats` also carries
`context_limit_tokens`, the engine's actual allocated `n_ctx`; that field is what makes
`WindowSource::EngineReported` reachable in section 3, and `context_pct()` derives used/limit from
the pair.

### 1.3 The chars/4 fallback

The characters-per-token heuristic survives only for providers that emit no `Usage` events at all:

```
prompt_tokens     ~= user_message.len() / 4          (floored at 1)
completion_tokens ~= accumulated_output_chars / 4    (floored at 1)
```

In that case `stats` is `None` on the `Done` event (`saw_usage.then_some(turn_stats)`), so a
consumer that needs real numbers can tell. `UsageStats` on its own cannot: it carries no
provenance flag, and nothing downstream reconstructs one.

**The UI does not distinguish them either, and an earlier version of this document said it did.**
`formatTokens` in `pond-desktop/src/components/UsageStatsCard.tsx` prefixes `~` to any count of
1000 or more, as a rounding marker for the `k` suffix, and prints counts below 1000 bare. It is not
an estimation marker and it never reads provenance. If estimate-vs-real needs to be visible, it has
to be plumbed; today it is not.

### 1.4 Persistence

`0016_session_usage.sql` adds the per-session totals:

```sql
ALTER TABLE sessions ADD COLUMN total_prompt_tokens     INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN total_completion_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN model_name TEXT;
```

`0029_message_token_counts.sql` adds the per-message columns:

```sql
ALTER TABLE session_messages ADD COLUMN prompt_tokens     INTEGER;
ALTER TABLE session_messages ADD COLUMN completion_tokens INTEGER;
```

Assistant rows carry the turn's prompt and completion sizes; user and tool rows stay `NULL`.

`SessionStorage::increment_usage()` adds tokens atomically after each chat response:

```sql
UPDATE sessions SET
    total_prompt_tokens     = total_prompt_tokens + ?,
    total_completion_tokens = total_completion_tokens + ?,
    model_name = COALESCE(?, model_name)
WHERE id = ?
```

---

## 2. Budgeting -- how much history fits (PAI-3 P2)

### 2.1 The `TokenCounter` port

`crates/pond-core/src/models/ports/token_counter.rs`:

```rust
pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> usize;   // excludes the per-message envelope
    fn is_exact(&self) -> bool;             // uses the ACTIVE model's vocabulary?
    fn name(&self) -> &'static str;         // "chars/4", "tiktoken-o200k"
}
```

Implementations must be cheap enough to call once per message per turn: this runs on the
hard-real-time trim path, which never blocks and never calls a model. Injecting the counter is what
lets `pond-core` do the arithmetic while carrying no tokenizer dependency of its own.

Two implementations exist:

| Implementation | Where | `name()` | `is_exact()` |
|---|---|---|---|
| `HeuristicTokenCounter` | `pond-core`, `models/services/context/token_counting.rs` | `chars/4` | `false` |
| `TiktokenCounter` | `pond-adapters-goose/src/token_counter.rs` | `tiktoken-o200k` | `false` |

`HeuristicTokenCounter` is `text.len() / CHARS_PER_TOKEN` with `CHARS_PER_TOKEN = 4` -- the exact
arithmetic the trimmer used to hardcode, so adopting the port changed no behaviour on its own.
`len()` is **bytes**, so multi-byte scripts are over-counted rather than under-counted; that is the
safe direction (it shrinks the history budget instead of overflowing the window) and a test exists
so nobody "fixes" it to `chars().count()` without realising they are removing a margin.

`PER_MESSAGE_TOKEN_OVERHEAD = 4` lives alongside it and is added by the **trimmer**, not the
counter: it is a property of the chat template's message envelope rather than of the text.

`TiktokenCounter` wraps `goose::token_counter::TokenCounter`, which is `tiktoken_rs::o200k_base`
(GPT-4o's vocabulary) with its own blake3-keyed LRU cache, so re-counting an unchanged history
every turn is cheap. `o200k_base` is embedded in the crate, so it works offline.

`GooseAdapter::token_counter()` builds the tiktoken counter once through a `OnceCell`, and on
failure logs `token counter unavailable, falling back to chars/4` and returns the heuristic. Both
budget paths (`trim_goose_history` and the session-hydration replay) take whichever they get.

### 2.2 No counter is exact, and the port says so on purpose

A counter is exact only when it uses **the active model's own vocabulary**. tiktoken over a Gemma
GGUF is a real BPE over the real bytes and still not exact, because the vocabularies differ.
Nothing may report `is_exact() == true` on the strength of being a real tokenizer alone.

tiktoken is nonetheless a large improvement on chars/4 exactly where the trim path spends its
budget: **tool results** (JSON is punctuation-dense, and `len()/4` misjudges it by an amount that
varies with the payload) and **non-Latin text** (`len()` counts bytes).

Exactness is deliberately deferred, with the cost written down in the adapter's module docs. The
active model's tokenizer lives behind `LlamaModel::str_to_token` inside `goose-local-inference`,
whose `llamacpp` module is private, so reaching it needs a fork-side patch carried in
`docs/goose-patch-management.md` and re-applied on every upstream rebase. `pond-inference` has a
genuine GGUF tokenizer, but it belongs to the quarantined `PondAgent` path (Q2-05) and uses its own
model instance -- using it here would load the model twice, which an 8 GB Jetson cannot afford.

### 2.3 The overshoot correction, and why it is load-bearing

Because no counter is exact, the estimate is corrected by measurement. `trim_history` takes the
previous turn's **real** prompt count (`last_real`, from the accounting side) alongside the
counter. When that real count overshot `CompactionProfile::usable_prompt_tokens()`, the effective
history budget shrinks by the overshoot, floored at `MIN_HISTORY_TOKENS = 64`:

```rust
budget = budget.saturating_sub(real - usable).max(MIN_HISTORY_TOKENS);
```

The floor exists so a catastrophic overshoot still leaves enough room that the assistant does not
forget the message it is answering. The correction converges in one turn -- which means the budget
is wrong for one turn every time the conversation's shape changes, and that is the accepted cost of
not having an exact counter.

Two further under-counts this bounds: `TrimMessage` is text-only, so a replayed image contributes
only its surrounding text rather than the ~250 tokens it really costs (`MAX_HISTORY_REPLAY_IMAGES`
is what keeps that to roughly one image's worth), and the image cap itself runs in the adapter
*after* `trim_history` returns.

The wiring: `GooseAdapter` records `turn_stats.prompt_tokens` into
`last_prompt_tokens_handle()` keyed by GIAP session id, **only when `saw_usage`** -- a chars/4
fallback number is never fed back as if it were measured. `trim_goose_history` reads it back for
the session it is about to trim.

---

## 3. The window the budget is measured against (PAI-3 P1)

A token count is only half of a budget; the other half is the window. Before PAI-3 P1 four code
paths answered "how big is this model's context window?" independently, and the worst of them --
the live history trimmer -- read `GOOSE_CONTEXT_LIMIT` from the process environment and fell back
to a hardcoded 8192.

`ContextGovernor` (`pond-core`, `models/services/context/context_governor.rs`) now owns that
answer. It is stateless: every result is a pure function of `ContextInputs`, which is what makes the
precedence testable without a live engine.

```rust
pub struct WindowResolution { pub tokens: usize, pub source: WindowSource }
```

| Rung | `WindowSource` | Input | `is_exact()` | Status |
|---|---|---|---|---|
| 1 | `EngineReported` | `TurnStats.context_limit_tokens`, wrapped in `EngineWindow { tokens, model }` | **yes** | live in `routes.rs` telemetry |
| 2 | `Registry` | pinned `context_size` from Goose's local model registry | **yes** | live in `GooseAdapter::resolve_window` |
| 3 | `CatalogRecord` | `ModelRecord.context_length` | no | wired, but no production caller supplies it -- see below |
| 4 | `Override` | `Settings.context_window_override` | no | live |
| 5 | `Heuristic` | caller-supplied `capability_window`, else `ModelCapabilities::from_model_name` | no | live |

Rung 3 has never been producible in production. Every `ContextInputs` construction site passes
`catalog_context_length: None`, and the only thing that has ever produced the variant is the
governor's own unit test -- a branch with full test coverage and no fixture production can build,
which is the `ProfileScope::Owner` shape exactly. PAI-3 P3 is populating
`ModelRecord.context_length` in the catalog providers; supplying it at the two live call sites is
P3b. The row above is wiring, not evidence that the field is consulted.

Rungs 1 and 2 outrank the user override deliberately: an override is a preference, an allocation is
a fact, and budgeting above the allocation only makes the engine truncate.

`EngineWindow` carries the model it was measured for, and the governor discards a reading whose
model does not match the active one. `TurnStats` is recorded per turn, so immediately after a model
swap the newest reading describes the model just left.

`WindowSource::is_exact()` is `true` for `EngineReported` and `Registry` only. It answers a
different question from `TokenCounter::is_exact()` -- *is this window a real allocation?* rather
than *is this count the model's own tokenization?* -- and today the two never both say yes, because
no counter says yes at all. `WindowSource::label()` (`engine` / `registry` / `catalog` / `override`
/ `heuristic`) exists so the provenance is displayable; nothing in `pond-desktop` reads it yet.

`ContextGovernor::prompt_window(provider, resolved)` clamps the **preamble** side to 8192 for
`local` and `gguf` providers and leaves HTTP providers alone. History budgets get the raw window.
Conflating the two silently grows the KV prefix on local providers, which is what an unclamped 32K
profile did: a 9.4K-token prompt and roughly 17 s TTFT for a one-line question.

### `GOOSE_CONTEXT_LIMIT` is written and never read

`goose_env_knobs` still exports it, because it flows into Ollama's `options.num_ctx` -- so the
reported limit, the request's `num_ctx` and the KV cache agree. Deleting the *write* would
desynchronise them. What P1 deleted is the two *reads*, and two guards keep them deleted:

- `this_module_does_not_read_the_environment` (in `context_governor.rs`) -- no `std::env` in the
  module body.
- `no_budget_path_reads_the_context_limit_from_the_environment` (in `goose_agent.rs`) -- no
  `env::var("GOOSE_CONTEXT_LIMIT")` outside the adapter's test module.

Both parse source text with `include_str!`, so both are the kind of check that can silently match
nothing. Only one has been watched fail: on 2026-08-06 `std::env::var("GOOSE_CONTEXT_LIMIT")` was
put back into `ContextGovernor::prompt_window`, and
`this_module_does_not_read_the_environment` failed with `context_governor must not read process
environment variables`, with the other sixteen governor tests still green. The twin in
`goose_agent.rs` has **not** been mutated, so treat it as unproven until it has been.

---

## 4. API

### `GET /api/v1/usage/summary`

Aggregates all sessions:

```json
{
  "total_prompt_tokens": 12500,
  "total_completion_tokens": 8200,
  "total_tokens": 20700,
  "session_count": 15,
  "cloud_input_price_per_million": 2.50,
  "cloud_output_price_per_million": 10.00
}
```

Pricing comes from settings (`cloud_input_price_per_million`, `cloud_output_price_per_million`).

### `GET /api/v1/sessions`

Includes per-session tokens:

```json
{
  "sessions": [{
    "id": "...",
    "title": "Weather chat",
    "total_prompt_tokens": 500,
    "total_completion_tokens": 300,
    "model_name": "gemma3:4b",
    "created_at": "...",
    "updated_at": "..."
  }]
}
```

---

## 5. Cost savings

The Dashboard "Usage & Savings" card compares on-device inference ($0) with cloud API pricing:

```
saved = (prompt_tokens     / 1,000,000) * cloud_input_price
      + (completion_tokens / 1,000,000) * cloud_output_price
```

Default comparison: GPT-4o ($2.50/M input, $10.00/M output). Configurable via settings.

| Setting | Default | Description |
|---------|---------|-------------|
| `cloud_input_price_per_million` | 2.50 | Cloud API input token price per million |
| `cloud_output_price_per_million` | 10.00 | Cloud API output token price per million |

Note that the totals being priced mix real provider counts and chars/4 fallbacks without
distinguishing them (1.3), so the savings figure inherits whatever error the fallback contributed.

---

## Related documents

- [Model Capabilities](./model_capabilities.md) -- where `context_window_tokens` sits in the
  governor's precedence, and why it is the last rung rather than the number budgets derive from.
- [PAI-3 Context governor](./pai/03-context-governor.md) -- the design, its invariants, and the
  phases still outstanding.
- [Inference Optimization](../developer/inference_optimization.md) -- the hardware reasons the
  preamble is clamped.
