# PAI-3 — Context governor: knowing the window, and using it

Requirement: *large context, and using each model's context window dynamically and to the fullest.*
Part of the [Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: none. Blocks [PAI-4](./04-smart-compaction.md), [PAI-5](./05-reasoning-and-thinking.md)
and [PAI-6](./06-multi-agent-orchestration.md).

Verified against code 2026-08-03.

---

## 1. What is true today

### 1.1 There is a real context subsystem, and it is good

`crates/pond-core/src/models/services/context/` — eight files, 3,108 lines, documented at
`context/mod.rs:1-22`. `CompactionProfile` (`context_budget.rs:30-60`) carries
`compaction_threshold`, `memory_token_budget`, `max_memory_fragments`, `system_prompt_budget`,
`history_token_budget`, `output_reserve_tokens` and the window it was derived from.

`output_reserve_tokens` is the sharpest piece of thinking in the file, and its doc comment
(`context_budget.rs:43-56`) is worth reading before changing anything here: on the Orin at
`n_ctx 4096` the preamble is ~3,250 tokens, two turns push the prompt to ~3,800, and a thinking
block of up to 306 tokens then overruns the window **mid-generation**. llama.cpp returns
`ContextLengthExceeded`, and Goose answers that by compacting reactively on a path that does *not*
consult `GOOSE_AUTO_COMPACT_THRESHOLD` — so GIAP's "we own compaction" setting cannot prevent it.
The user watches their history get replaced by a summary and their answer truncated to one
character.

`turn_trimmer.rs` (the deterministic in-turn trimmer) and `SessionSummaryService` (the idle rolling
summary) are likewise sound and stay.

### 1.2 Four code paths disagree about the window size

| Path | Location | Precedence |
|---|---|---|
| `GooseAdapter` | `goose_agent.rs:907-949` | pinned registry `context_size` > `context_window_override` > (`local`/`gguf` → hardcoded `32768`; HTTP → name heuristic) |
| `PondAgent` | `pond-agent/src/agent.rs:375-379` | `min(override, caps.context_window_tokens)` |
| API telemetry and monitor | `routes.rs:1674-1685`, `:1722-1727` | `TurnStats.context_limit_tokens` > `override` > `caps` |
| **The live trimmer** | `goose_agent.rs:1478-1481` | `std::env::var("GOOSE_CONTEXT_LIMIT")` **> hardcoded `8192`** |

The fourth is the bug. `trim_goose_history` — the function that decides what the model actually
sees — reads an environment variable and falls back to 8192. `apply_goose_env_knobs`
(`goose_agent.rs:797-844`) does set that variable, but only when its signature changes, so the
trimmer's view is a side effect of an unrelated code path rather than a value it asked for.

### 1.3 Token counting is chars/4

`turn_trimmer.rs:89-91` estimates `text.len()/4 + 4` per message. There is no tokenizer in
`pond-core`. The estimate is corrected by feedback: when the previous turn's real prompt count
exceeded `usable_prompt_tokens()`, the budget shrinks by the overshoot, floored at
`MIN_HISTORY_TOKENS = 64` (`:39,130-141`). It converges in one turn, which means it is wrong for
one turn, every time the conversation shape changes.

Real counts *are* available — migration `0029_message_token_counts` stores per-message
`prompt_tokens`/`completion_tokens`, and `TurnStats.context_limit_tokens` carries the engine's
actual allocated `n_ctx`. (`docs/architecture/token_tracking.md` said otherwise until the
2026-08-03 documentation-debt pass; P6 then rewrote it around the accounting/budgeting split this
workstream forced into the open.)

### 1.4 Per-model metadata exists in three places, none of them usable

1. **Goose's local model registry** — `registry_context_size` (`goose_agent.rs:899-905`) reads
   `entry.settings.context_size`. This is the only *real* per-model context metadata and it lives
   inside the submodule.
2. **`ModelCapabilities::from_model_name`** (`model_capabilities.rs:182-196`) — a string heuristic:
   `gemma-4*` → 128,000; `llama-3` → 8,192; `qwen`/`mistral` → 32,768; **everything else → 4,096**.
3. **`ModelRecord.context_length: Option<u32>`** (`ModelRecord` in
   `models/domain/model_record.rs`) — exists in the schema, persisted by
   `SqliteModelRepository::upsert` and read back by `row_to_record`. **No budget code consults it.**

   *Corrected 2026-08-06.* The original claim, "written as `None` by every production path
   (`model_service.rs:383,532`, `routes.rs:3174`)", was wrong twice over. Both `model_service.rs`
   sites are inside the `#[cfg(test)]` module — test fixtures, not production paths — and
   `gguf_record` in `composite_model_catalog_provider.rs` writes `Some(e.context_length)` for all
   twenty curated GGUF entries. What is actually true is an **asymmetry**: the GGUF table populates
   the field, `llamafile_record` and `ollama_entry_to_record` write `None`, and the Whisper / Piper /
   embedding rows write `None` correctly because the field is meaningless for them.

   The value being unread is what let the data rot unnoticed: every Gemma 4 entry declares `8192`,
   copy-pasted from the Gemma 2 rows above it, against a real declared window of `131072` (verified
   2026-08-06 against `model_info["gemma4.context_length"]` from a live `POST
   localhost:11434/api/show`).

There is no models catalog JSON anywhere in the repository.

### 1.5 The tiers are coarse, and the local clamp is deliberate

`CompactionProfile::from_context_window` (`context_budget.rs:78-126`) has four hardcoded tiers:
≤4096, ≤12288, ≤65536, and above. A 24K model and a 64K model get identical budgets.

Separately, `prompt_budget_ctx` (`goose_agent.rs:879+`) clamps the *prompt-side* budget to the 8K
tier for local providers even when the KV cache is larger. The doc comment records why: an
unclamped 32K profile produced a 9.4K-token prompt and roughly 17 s TTFT for a one-line question.

---

## 2. The gap

"Use each model's context window dynamically and to the fullest" currently means: guess the window
from the model's name, unless an environment variable happens to be set, and estimate occupancy by
dividing character counts by four. Every downstream feature in this programme — compaction,
thinking budgets, per-subagent allocation — is arithmetic on those two numbers.

---

## 3. Design

### 3.1 One governor, one precedence

New service `pond-core/src/models/services/context/context_governor.rs`:

```rust
pub struct ContextGovernor { /* … */ }

pub struct WindowResolution {
    pub tokens: usize,
    pub source: WindowSource,   // EngineReported | Registry | CatalogRecord | Override | Heuristic
}
```

One precedence, used by the adapter, the trimmer, telemetry and the monitor alike:

1. **Engine-reported `n_ctx`** — ground truth. The engine knows what it allocated; it is already
   surfaced as `TurnStats.context_limit_tokens`.
2. **Pinned registry `context_size`** — authoritative for GGUF models because *it is the
   allocation*, which is why it currently outranks the user override and must keep doing so.
3. **`ModelRecord.context_length`** — for catalog models, once actually populated (3.3).
4. **`context_window_override`** — the escape hatch for memory-constrained deployments.
5. **Name heuristic** — last resort, and the only path that may return the conservative 4,096.

`WindowSource` is carried, not discarded. "Why does this model think it has 4K?" is a question the
Models UI should be able to answer without a debugger.

The trimmer stops reading the environment. That single change is the highest-value fix in this
workstream: today a Jetson with a correctly configured 3K window can be trimming against a
phantom 8,192.

### 3.2 Real token counting

New port `models/ports/token_counter.rs`:

```rust
pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> usize;
    /// Cheap enough to call per message per turn?
    fn is_exact(&self) -> bool;
}
```

Backed by the GGUF tokenizer already loaded inside `pond-inference` for local models; chars/4
becomes the documented `is_exact() == false` fallback for HTTP providers where no tokenizer is
available.

The existing overshoot-feedback path stays as a safety net rather than the primary mechanism — it
is what protects against a tokenizer/engine mismatch, and it costs nothing when the counter is
exact.

### 3.3 Make `context_length` real

Catalog providers populate `ModelRecord.context_length` instead of `None`. It surfaces in the Models
UI beside the existing capability badges, and it feeds precedence rung 3. This turns a dormant
column into the answer for HTTP and Ollama models, where no registry entry exists.

`docs/architecture/model_capabilities.md` is refreshed at the same time. The missing sixth field
(`tool_calling`) was added on 2026-08-03; P6 fixed what that pass left behind — a detection table
with no `tool_calling` column, a `qwq` row claiming 32K when the code gives it 4,096, a vision row
predating `name_implies_vision`, and `trim_to_budget_for_model` cited as live when nothing outside
its own tests calls it.

### 3.4 Asymmetric budgets — how "to the fullest" is actually achieved

The instinct on reading "use the window to the fullest" is to remove the local 8K clamp. That would
be wrong, and the measurement in `goose_agent.rs:879+` says why: 9.4K prompt tokens and 17 s TTFT
for a one-line question.

The insight is that a context window has two halves with opposite cost curves:

- **The preamble** — system prompt, tool schemas, injected memories. Paid on *every* turn, and it is
  the KV prefix, so growing it grows TTFT permanently.
- **The working set** — conversation history and tool results. Also paid every turn, but it is what
  makes the assistant feel like it remembers you, and it is what gets thrown away first today.

So: **growing the window buys working set, not preamble.** The preamble budget stays clamped near
the 8K tier's numbers regardless of window size; `history_token_budget` scales with the window.
That is the concrete meaning of "dynamically and to the fullest" on this hardware, and it is
compatible with everything Phase D landed on tool narrowing.

### 3.5 A continuous profile function

Replace the four hardcoded tiers with a continuous function plus named floors, so 24K and 64K models
stop sharing a bucket:

```
output_reserve  = clamp(window * 0.10, 768, 4096)
system_budget   = clamp(window * 0.25, 1500, 10_000)     # preamble, deliberately capped low
memory_budget   = clamp(window * 0.04, 200, 4000)
history_budget  = window - output_reserve - system_budget - memory_budget - safety
threshold       = 0.60 + 0.20 * saturating_fraction(window)
```

The existing tier values become the test fixtures: the function must reproduce today's numbers at
4,096 / 12,288 / 65,536 / 128,000 within a stated tolerance, so this is a refactor with a
regression net rather than a retune.

> **Corrected 2026-08-06, when P4 tried to implement it.** The formula above does **not** reproduce
> the tier values, and no honest tolerance covers the gap: at 8,192 it gives a 819-token output
> reserve against the tier's 1,024, and at 128,000 a residual history budget of 109,904 against the
> tier's 80,000 — 37% out. A formula and a fixture set cannot both be authoritative, and the phase
> text names the fixtures as the regression net, so the fixtures won.
>
> What landed is **piecewise-linear interpolation over an anchor table whose anchors are the tier
> values themselves**, flat below the first and above the last. It reproduces the tiers by
> construction rather than approximately by luck, and it still separates 24K from 64K, which is the
> thing this section wanted. The anchor set is **six**: the four tier boundaries plus 8,192 and
> 32,768, neither of which is a boundary but both of which are live windows the system actually
> produces. See P4 in section 4 for why removing either is a regression.
>
> "Within a stated tolerance" was the part that could not survive contact. Nobody had stated one,
> and any tolerance loose enough to admit this formula is loose enough to admit a memory budget that
> drops from 500 tokens to 350 on every on-device turn.

### 3.6 One more thing the governor makes possible

Once occupancy is measured rather than estimated, `ContextHealth.should_compact`
(`context_monitor.rs:155`) becomes trustworthy enough to act on. That is [PAI-4](./04-smart-compaction.md).

---

## 4. Phases

- **P1 — LANDED.** `ContextGovernor` + `WindowResolution` + `WindowSource`; every one of the four
  call sites repointed. The trimmer's `env`-with-8192 read is deleted (there were **two** such
  reads: `trim_goose_history` and session hydration). `prompt_budget_ctx` moved out of the adapter
  into `ContextGovernor::prompt_window` unchanged. `ContextInputs` gained `capability_window` so
  `routes.rs` keeps its live-capability fallback instead of regressing to the name heuristic.
  `GOOSE_CONTEXT_LIMIT` is still **written** — it feeds Ollama's `options.num_ctx` — but no longer
  read, guarded by a regression test.
- **P2 — LANDED, with its premise corrected.** `TokenCounter` port; `HeuristicTokenCounter`
  (chars/4) as the declared fallback; a tiktoken-backed adapter on the live path. **Not** the
  GGUF-backed adapter this phase originally specified — that tokenizer is unreachable (private
  module in the fork; `pond-inference`'s belongs to the quarantined agent and would double-load the
  model), so no counter reports `is_exact()`, and the feedback correction below stays load-bearing
  rather than becoming a safety net. Exact counting is deferred with the fork-patch cost written
  into `pond-adapters-goose/src/token_counter.rs`.
- ~~**P2** `TokenCounter` port; GGUF-backed adapter; chars/4 as the declared fallback.~~ Feedback
  correction retained.
- **P3 — PARTIAL. Data landed; the wiring and the UI are BLOCKED.** `ModelRecord.context_length` is
  now populated by every catalog provider that can answer: the llamafile table gained the field, the
  Gemma 4 rows were corrected from a copy-pasted `8192` to the declared `131072`, and
  `OllamaCatalogProvider` reads the real window from `POST /api/show`'s `model_info` map — the
  provider class rung 3 exists for, and the reason it has never been reachable. Rung 3 itself gained
  the clamp it was missing: a catalog value is a *declared maximum*, not an allocation, so for a
  local provider it is bounded by `UNPINNED_LOCAL_CEILING`. Without that, populating the field would
  have handed the trimmer a 128K history budget on a Mac that allocated 32K.

  **Still not reachable in production.** All three `ContextInputs` construction sites —
  `goose_agent.rs::resolve_window_with`, `routes.rs`'s `turn_context_limit`, and the quarantined
  `pond-agent/src/agent.rs` — still pass `catalog_context_length: None`, so `WindowSource::CatalogRecord`
  remains a rung nothing can produce. The two live ones are in files a concurrent workstream holds.
  The Models UI half is blocked on the same file: `record_to_dto` and `ModelStatusEntry` would both
  have to change, and `record_to_dto` lives in `routes.rs`. Deferred to **P3b**, which is exactly:
  supply the record's `context_length` at the two live call sites, add it to `ModelStatusEntry`, and
  render it in `Models.tsx` beside `CapabilityBadges` — which today infers the window from the model
  *name* in the frontend, a third copy of the heuristic this workstream exists to delete.
- **P4 — LANDED 2026-08-06, with six anchors rather than a formula.** `from_context_window` is
  piecewise-linear interpolation over `PROFILE_ANCHORS`, flat below the first anchor and above the
  last. Its signature, its fields and `usable_prompt_tokens()` are unchanged, so all four production
  call sites and the quarantined fifth compile untouched — which is what let this land while a
  concurrent session held `turn_trimmer.rs` and `prompts.rs`.

  **The design's own formula in 3.5 could not be implemented, and the fixtures are why.** It gives a
  819-token output reserve at 8,192 against the tier's 1,024, and a residual history budget of
  109,904 at 128,000 against the tier's 80,000 — 37% out. A formula and a fixture set cannot both be
  authoritative. Interpolating between the tier values reproduces them *by construction* rather than
  approximately by luck, and still delivers what the tiers could not: 24K and 64K stop sharing a
  bucket.

  **Six anchors, not four, and shrinking it back to four is a regression.** 8,192 and 32,768 are not
  tier boundaries, but 8,192 is what `ContextGovernor::prompt_window` hands every local provider, so
  it is the most-executed window in the system — every Jetson and macOS Metal turn passes through it
  for the compact-prompt decision and memory injection. Interpolating it would have cut
  `max_memory_fragments` from 5 to 4 and `memory_token_budget` from 500 to 350 on every on-device
  turn: a real regression wearing a refactor's clothes, which is the exact hazard the phase brief
  named. Removing that one anchor fails four tests, **two of them pre-existing**
  (`compaction_profile_macos_8k`, `available_history_chars_subtracts_overhead`) — so the anchor is
  load-bearing for shipped behaviour, not just for the new curve's own tests. Verified by mutation,
  not asserted.

  `use_compact_prompt` is deliberately **not** interpolated. Every other field is a budget answering
  "how much"; that one is a format switch answering "which", and a continuous curve through a
  boolean has no meaning. Its 12,288 boundary is unchanged.

  What actually improves: the tier constants satisfy
  `reserve + system + memory + history <= window` at exactly their four edges and at **none** of the
  195,905 windows between them — at 12,289 they declare 29,548 tokens against a 12,289-token window.
  The curve's over-commitment set is a strict subset of the tiers' (2,116 windows, down from
  54,245) and worst-case over-commitment falls from 2.404x to 1.041x.
- **P5** Asymmetric budgeting: preamble capped, working set scaled. Measure TTFT before and after on
  both Mac and Orin — this phase is only correct if TTFT is flat and retained history grows.
- **P6 — LANDED (2026-08-06).** `token_tracking.md` rewritten around the split this workstream
  forced into the open: **accounting** (real provider `Usage` aggregated into `TurnStats`,
  per-session totals, savings) and **budgeting** (`TokenCounter`, `CompactionProfile`, the trimmer),
  which meet at exactly one place, the overshoot correction. `model_capabilities.md` rewritten
  around the demotion of `context_window_tokens` to the governor's last rung. Four claims were false
  rather than merely incomplete, and each is named in the new text so the correction cannot be
  quietly reversed: the UI does not mark estimates with `~` (it is a rounding marker on counts of
  1000 or more); `trim_to_budget_for_model` has no production caller; `qwq` resolves to 4,096
  because the context arm matches `qwen`, which `qwq` does not contain; and
  `GET /models/capabilities` does return `tool_calling`, because the handler serialises the struct
  whole. Recorded, not fixed: `Models.tsx`'s `inferCapabilities` has drifted from `from_model_name`
  in both directions — no E1B exclusion, no `gemma-3n` spellings, no `vl`/`vision` segment rule — so
  the badges disagree with the backend on precisely the model the E1B exclusion exists for. Both
  documents describe a mid-programme state and say so; the rung-3 rows are what P3b moves.

---

## 5. Invariants

1. The preamble is the KV prefix. Growing the window must not grow it.
2. `output_reserve_tokens` is never zero and never optional. It is the only thing standing between a
   long thinking block and a mid-generation context overrun.
3. Registry `context_size` outranks the user override, because it is the allocation and not a
   preference.
4. `WindowSource` is always available for display. A number without a provenance is unsupportable.
5. No budget path may read process environment variables. Configuration flows through the governor.

---

## 6. Deliberate deferrals

- **A bundled models catalog JSON.** Attractive for HTTP providers, but it dates instantly and
  duplicates the registry. Populating `ModelRecord.context_length` from live catalog fetches is the
  better shape.
- **Per-session window overrides.** No demonstrated need; adds a second axis to every budget test.
- **Tokenizer for HTTP providers.** Would mean shipping tiktoken-equivalents per vendor. The
  overshoot-feedback correction already bounds the error there.

---

## 7. Verification

- **Unit** — precedence table, one case per rung including the tie-breaks; the continuous profile
  function against the four existing tiers as fixtures.
- **Regression** — a test asserting no file under `models/services/context/` and no adapter budget
  path calls `std::env::var`.
- **Measured** — same question, fresh session, on Mac (gemma-4-E2B-it Q4_K_M) and Orin: record
  `prompt_tokens`, `ttft_ms`, `prefill_ms` and retained history turns before and after P5. The
  success criterion is *flat TTFT with more retained history*, not a bigger prompt.
- **Integration** — set `context_window_override` below the registry size and assert the registry
  still wins, with `WindowSource::Registry` reported.
- **End to end** — run a long conversation on a 3K Jetson profile and assert no
  `ContextLengthExceeded` and no reactive Goose compaction in the logs.
