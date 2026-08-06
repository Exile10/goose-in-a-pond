# PAI-4 — Smart compaction: model, time, and cache age

Requirement: *smart compaction based on different models, time and cache age.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-3](./03-context-governor.md) — compaction is arithmetic on the window.

Verified against code 2026-08-03.

---

## 1. What is true today

### 1.1 Hybrid compaction works and stays

`hybrid_compaction_enabled` defaults **true** (`settings.rs:425-434`). Two halves:

- **The deterministic trimmer** — `turn_trimmer.rs`, "the hard-real-time half… Never calls a model,
  never blocks" (`:1-31`). Strips stale `<system-context>` blocks from prior user messages,
  truncates oversized tool results head-and-tail, splices the rolling summary at the front, and
  drops the **oldest complete turns** until the estimate fits. A turn is a user message plus
  everything until the next one (`last_turn_start`, `:95-100`), so tool results never orphan.
- **The idle rolling summary** — `SessionSummaryService` (`shared/services/session_summary.rs`).
  Runs after `summary_idle_secs`, never at startup, cancelled by a new turn.
  `KEEP_RECENT_MESSAGES = 6`, `MIN_UNSUMMARIZED_MESSAGES = 4`. Result lands in
  `sessions.rolling_summary` (migration `0030`) and the trimmer splices it as
  `<conversation-summary>`. **Turns read it; they never wait for it.**

GIAP also disables Goose's competing machinery on this path, in `goose_env_knobs`. Note the two
knobs are gated differently, which I originally ran together: `GOOSE_AUTO_COMPACT_THRESHOLD = "1.0"`
is set for **every** provider whenever `hybrid_compaction_enabled` is on, while
`GOOSE_TOOL_PAIR_SUMMARIZATION = "false"` is `local`/`gguf` only — the code's reason being that "an
HTTP provider's spare capacity is not ours to save".

### 1.2 Compaction is model-tier-aware and nothing else

`CompactionProfile::from_context_window` varies *budgets* by window size. It does not vary
*strategy*, and nothing anywhere considers how old a turn is or what state the KV cache is in.

### 1.3 `ContextCompactor` is dead code

277 lines of LLM summarisation (`context/context_compactor.rs`): `needs_compaction`, `compact`,
`summarise`, an `[Earlier conversation summary]` splice, and a `trim_to_budget` fallback on LLM
error. It is stored on `ChatService` as `compactor: Option<ContextCompactor>` (`chat.rs:197`),
initialised `None` (`:270`), settable via `with_context_compactor` (`:424-425`) — and **never
read**. No caller of `with_context_compactor` exists anywhere in `crates/`.

> **Deleted 2026-08-06, and the deletion is correct.** `context_compactor.rs` and
> `compact_encoding.rs` are gone and no reference to `ContextCompactor` remains anywhere in
> `crates/`. This section is kept because the *diagnosis* was right and the phase list was built on
> it; only the disposition changed, from "revive" to "rewrite".
>
> The reason is PAI-3, not tidiness. The deleted implementation was built on
> `USABLE_HISTORY_CHARS` and its own `const CHARS_PER_TOKEN: usize = 4` — precisely the estimator
> PAI-3 P2 spent a phase replacing with the `TokenCounter` port, and precisely the kind of second
> copy PAI-3 P1 exists to prevent. Reviving it would have re-imported the heuristic two landed
> phases removed, into the one tier where an accurate count is affordable. Dead code that has
> drifted three phases behind the live path is not an asset waiting to be switched on.

### 1.4 `should_compact` is computed and ignored

`ContextMonitor::check_context_health` (`context_monitor.rs:116-186`) returns `should_compact` when
utilisation exceeds 75% or fewer than three turns remain at the current growth rate. The API emits a
`context_warning` SSE event (`routes.rs:1748-1757`) and logs. **Nothing acts on it server-side**, and
`reset_session` (`:187`) is never called from any handler.

### 1.5 The cost of moving the prefix is known and unmodelled

The repository already measured it: a 78-character system-prompt delta cost every session a full
re-prefill on its second turn — 3.7 s on the Orin (`goose_agent.rs:1000-1011`). Phase D made tool
selection *sticky* per session for exactly this reason. But the compaction path has no notion of
the prefix's state at all; it decides purely on token arithmetic.

Also relevant: any turn containing an image bypasses KV retention entirely, which is why
`MAX_HISTORY_REPLAY_IMAGES = 1` (`context/image_history.rs`).

---

## 2. The gap

Compaction today asks one question — "does it fit?" — and answers it with one strategy. The
requirement names three axes it should also consider, and each corresponds to a real cost the
current design pays blindly:

- **Model** — a 20 tok/s on-device model and a hosted 128K model should not compact the same way.
- **Time** — a turn from three days ago is not worth the same tokens as one from five minutes ago,
  and a session resumed after a gap is a free opportunity nobody takes.
- **Cache age** — compaction that moves the prefix costs seconds of re-prefill; compaction performed
  when the cache is *already* cold costs nothing.

---

## 3. Design

### 3.1 Model axis — vary strategy, not just budgets

| Model class | Strategy |
|---|---|
| Small on-device (`local`/`gguf`, ≤ 12K window) | Deterministic trim, plus the idle rolling summary. No **blocking** LLM in the compaction path — a summarisation stall at 20 tok/s is a user-visible hang, which is precisely why hybrid compaction exists. |
| Medium (32K, Ollama/llamafile) | Deterministic trim + idle rolling summary (today's behaviour). |
| Large / HTTP (≥ 64K) | Deterministic trim + idle summary + **LLM re-summarisation of the summary itself** when it grows stale. Here the summarisation call is cheap and off the critical path. |

> **Corrected 2026-08-06 by P1, in two places.**
>
> **The small row said "deterministic trim only. No LLM in the compaction path."** The stated reason
> is a stall at 20 tok/s, and the idle rolling summary cannot produce one: it runs only after
> `summary_idle_secs`, a new turn cancels it, and turns read `sessions.rolling_summary` without ever
> awaiting it (1.1; invariant 5). Reading the row literally would have removed the rolling summary
> from the device whose window runs out first, to prevent a hang that path structurally cannot cause.
> What the small tier really excludes is a model call the *compaction* has to wait for — which is the
> large tier's re-summarisation, and nothing else here. The idle summary does carry a real on-device
> cost, but a different one: it evicts the prefix cache, so the next turn pays a prefill it would not
> have. That is P5's arithmetic, it applies to the medium tier just as much, and it needs a
> measurement rather than a flag flipped here.
>
> **The three rows each mix a window size with a provider**, and two live configurations fall between
> them: an 8K *hosted* model, and a 131,072-token *Ollama* model on the Orin, which PAI-3 P3 made
> reachable by teaching `OllamaCatalogProvider` to read `context_length` from `/api/show`.
> `model_class.rs` implements the table as the two independent questions it is made of — a window
> bracket (`SMALL_WINDOW_CEILING` / `LARGE_WINDOW_FLOOR`) and an on-device provider set. Every row
> above is reproduced; the small hosted model takes the small strategy (at 12K there is no span worth
> re-summarising), and the 131K Ollama model takes the *medium* strategy with the *large* budgets,
> because the window is generous and the compute is not.

The large-model tier is where re-summarisation belongs. It is a problem the on-device tier does not
have, and the answer to why nothing was ever wired: nobody had a tier where it was safe.

> **Corrected 2026-08-06.** This paragraph said the dead `ContextCompactor` would be "revived rather
> than deleted". It has since been deleted, correctly — see 1.3. What this tier needs is written
> fresh against the landed governor: `TokenCounter` for the measurement, `CompactionProfile`'s
> interpolated curve for the budget, and `ModelClass` for the gate. The salvage from the old file is
> its *shape*, which was sound and is worth restating so it is not rediscovered: summarise the older
> span, splice the result back as a single message, keep the most recent turns verbatim, and fall
> back to a deterministic trim whenever the LLM call fails so a chat turn is never interrupted by
> compaction.

### 3.2 Time axis — age-weighted retention and compact-on-resume

**Age-weighted retention.** The trimmer currently treats every retained turn as equally valuable and
drops strictly oldest-first. Add recency tiers:

| Age | Treatment |
|---|---|
| Current session, recent turns | Verbatim |
| Same day | Eligible for tool-result truncation and `<system-context>` stripping first |
| Older than `compaction_verbatim_days` | Represented by the rolling summary only |

This is not a new mechanism — it is a priority order over mechanisms the trimmer already has. It
matters most for the long-lived "household" sessions PAI-7 will create, which are never closed.

**Compact-on-resume — the highest-value time behaviour, and it is free.** A session reopened after a
gap is about to pay a full prefill anyway (the KV cache is long gone). Today compaction happens
*during* the first turn back, while the user waits on a token stream. Doing it on resume, before the
first user message arrives, costs the user nothing:

```
session resumed && idle_gap > resume_compaction_idle_secs
    → run full compaction (and, on large models, re-summarisation) before the first turn
```

The trigger reuses the pattern already proven in `user_data/services/consolidation_schedule.rs` —
a pure `should_run` gate with a startup guard — rather than inventing new scheduling.

> **Corrected 2026-08-06 by P4, on what "run full compaction" can mean today.** The pseudo-code
> reads as though a single compaction pass exists to be invoked. It does not: hybrid compaction is
> two halves with opposite timing. The deterministic trimmer is a function of the turn being
> assembled, so there is nothing to pre-run and nothing to save — it is cheap by construction. The
> half worth moving off the critical path is the rolling summary, and there the gap is real: the
> idle refresh loop skips any session whose `updated_at` predates process start, so a conversation
> from before the last restart keeps a summary frozen at the restart and the trimmer splices that
> stale summary into every turn until four new messages accumulate. Compact-on-resume refreshes it
> at the reopen. The large tier's re-summarisation is the third thing this trigger will run, once
> P2 exists.
>
> **`resume` is a user action, not an elapsed duration.** The startup guard the design points at has
> a specific shape here that the one-line rule hides: at boot every stored session satisfies
> `idle_gap > resume_compaction_idle_secs`, so a gate keyed on the gap alone would compact the whole
> history store on startup. `ResumeGateInputs::reopened` is the guard, and it can only be set by a
> request somebody made.

### 3.3 Cache-age axis — the core insight

Introduce prefix-cache state as an *input* to the compaction decision:

```rust
pub struct PrefixCacheState {
    pub built_at: Instant,
    pub hash: u64,
    pub turns_served: u32,
    pub invalidated_by: Option<InvalidationReason>,
    // ProviderRebuilt | ModelSwapped | SessionResumed | MultimodalTurn |
    // ToolSetChanged | PromptChanged
}
```

The rule:

> **Recompact aggressively when the cache is already lost; otherwise prefer edits that keep the
> prefix byte-identical.**

Because the prefix is *already* gone after a provider rebuild, a model swap, a session resume, or a
multimodal turn, a full recompaction at that moment is genuinely free — the re-prefill is happening
regardless. Conversely, on a warm cache mid-conversation the trimmer should prefer operations that
touch only the tail-adjacent middle (tool-result truncation, dropping a whole old turn) over anything
that perturbs the front.

This also explains an existing accepted cost more precisely: Phase D noted that two sessions
alternating on one model diverge at the tools block and re-prefill more. With `PrefixCacheState`,
that alternation becomes *observable* — `invalidated_by: ToolSetChanged` — and therefore an
opportunity to recompact rather than an invisible tax.

`turns_served` is the value signal: a prefix that has served 30 turns is worth protecting; one that
has served two is not.

### 3.4 Act on `should_compact`, between turns

`ContextHealth.should_compact` becomes actionable — but compaction runs **between** turns, never
during one, consistent with the programme's invariant 2. The existing `context_warning` SSE keeps
flowing to the client. `ContextMonitor::reset_session` gets called when a session is cleared or its
history is compacted, so the growth-rate samples stop describing a conversation that no longer
exists.

### 3.5 An explicit compaction endpoint

`POST /api/v1/sessions/{id}/compact` — no such route exists today. Returns what changed: turns
dropped, tokens reclaimed, whether the summary was refreshed, and whether the prefix survived. Users
who understand their assistant's memory pressure should be able to act on it, and it makes every
phase above manually testable.

### 3.6 What is not changing

The trimmer stays deterministic and non-blocking. Tool results keep travelling with their turn. The
rolling summary stays idle-only and cancellable. Images stay out of the trimmer
(`turn_trimmer.rs:18-30` explains why, and adding a second image rule there would drift from
`image_history.rs`).

---

## 4. Phases

- **P1 — LANDED 2026-08-06, as two axes rather than three rows.**
  `models/services/context/model_class.rs`: `ModelClass { Small | Medium | Large }`,
  `CompactionStrategy { deterministic_trim, idle_rolling_summary, llm_resummarisation }`,
  `ModelClass::classify(provider, window)` and `ModelClass::from_resolution(provider,
  &WindowResolution)`. Domain only — no call site yet, same shape as PAI-3 P1, and **P2 is the first
  consumer**. 15 unit tests.

  **The table in 3.1 could not be implemented as written, and the reason is a real gap rather than a
  wording quibble.** Each of its three rows names a window size *and* a provider, so two
  configurations that exist today fall between them: an 8K hosted model, and a 131,072-token Ollama
  model on the Orin — which PAI-3 P3 made genuinely reachable when it taught `OllamaCatalogProvider`
  to read `context_length` from `/api/show`. The implementation splits the rows into the two
  independent questions they are made of, and both cells then have answers. The window brackets are
  `SMALL_WINDOW_CEILING = 12_288` and `LARGE_WINDOW_FLOOR = 65_536`, which are not new numbers:
  12,288 is `use_compact_prompt`'s step and `PROFILE_ANCHORS`' third point, 65,536 is its fifth.

  **`runs_on_this_device` is deliberately NOT `ContextGovernor::is_local_provider`, and conflating
  them would be the defect this phase exists to avoid.** The governor's predicate answers "is the
  preamble re-prefilled here every turn?", which is true for the in-process engine and false for
  Ollama. This one answers "would an extra model call compete with the turn the user is waiting
  on?", which is true for Ollama and llamafile as well — on the Orin they are HTTP to `127.0.0.1`
  and the tokens come off the same 102 GB/s. So `ON_DEVICE_PROVIDERS` is
  `local | gguf | ollama | llamafile`, matched case-insensitively, and it is a **deny-list for the
  large tier**: it is safe when too wide and unsafe when too narrow, which is why a provider that
  *could* point at another machine (`OLLAMA_HOST`) is kept inside it. Only `Large` unlocks a
  mechanism, so every fallback resolves downward.

  **Where this contradicts the document, and the contradiction is deliberate.** 3.1's small-tier row
  and invariant 3 both say no LLM in the compaction path on-device. `strategy()` nevertheless
  reports `idle_rolling_summary: true` for `Small`. The stated reason for the prohibition — "a
  summarisation stall at 20 tok/s is a user-visible hang" — is a hazard the idle rolling summary
  structurally cannot produce: it runs only after `summary_idle_secs`, it is cancelled by a new
  turn, and a turn reads `sessions.rolling_summary` without ever awaiting it (1.1, and invariant 5
  says the same). Setting the flag false would have removed the rolling summary from the one device
  whose window runs out first, to prevent a stall that path cannot cause — and the phase text is
  explicit that P1 preserves today's behaviour on the small and medium tiers. There *is* a real
  on-device cost to an idle summarisation: it evicts the prefix cache, so the next turn pays a
  prefill it would not have. That is cache-age arithmetic, it applies to the medium tier equally,
  and it belongs to **P5** with a measurement behind it. Both 3.1 and invariant 3 now carry that
  correction; `strategy()`'s doc comment carries the argument.

  **Consequence: `Small` and `Medium` select the same strategy today**, and a test asserts it rather
  than leaving it to be discovered as a bug. The classes are not redundant — they are the gate P3's
  `compaction_verbatim_days` and P5's cache rules key on — but in P1 the only tier whose strategy
  differs is `Large`.

  **What the compiler could not have caught.** `CompactionProfile` gained no field. `turn_trimmer.rs`
  constructs it with exhaustive literals in two test helpers, so a new field would have broken a file
  a concurrent session was expected to hold — the same structural collision that nearly destroyed the
  encrypted secret store in the PAI-2 batch. Keeping the profile's signature untouched is what let
  PAI-3 P4 land under the same conditions, and it is why the class is derived rather than stored.
- **P2 — RESPECIFIED 2026-08-06. Write a large-tier compaction strategy; do not revive anything.**
  The phase used to read "revive `ContextCompactor`". That file was deleted on 2026-08-06 and the
  deletion is right (see 1.3): it carried its own `CHARS_PER_TOKEN = 4` and `USABLE_HISTORY_CHARS`,
  the estimator PAI-3 P2 replaced with the `TokenCounter` port, so reviving it would have re-imported
  a heuristic two landed phases removed — into the one tier that can afford an accurate count.

  What P2 builds instead: LLM re-summarisation for the large tier, written against what has landed —
  `TokenCounter` for measurement, `CompactionProfile`'s interpolated curve for the budget,
  `ModelClass` for the gate, an `LlmProvider` for the call. Keep the old file's shape, which was
  sound: summarise the older span, splice it back as one message, keep recent turns verbatim, and
  fall back to a deterministic trim on any LLM failure so a turn is never interrupted.

  **And add the first test that actually executes it** — that clause survives the respec unchanged,
  because it is the reason the original was dead. 277 lines with no caller passed every gate this
  repo has for as long as it existed.
- **P3** Age-weighted retention priority in the trimmer, with `compaction_verbatim_days` as a
  headless setting.
- **P4 — LANDED 2026-08-06, with a call site, and doing less than "full compaction" for a
  reason.** `models/services/context/resume_compaction.rs`: `ResumeGateInputs`,
  `SkipReason { Disabled | NotAReopen | NoPriorHistory | StillWarm | NoSummariser }`,
  `GateDecision`, `should_run`, `idle_threshold_from_secs`, `idle_gap_since`. Fourteen unit tests.
  `resume_compaction_idle_secs` is a new headless setting, defaulting to
  `RESUME_IDLE_THRESHOLD_SECS` so the setting's default and the gate's cannot drift. The caller is
  `spawn_resume_compaction` in `routes.rs`, fired from `GET /api/v1/sessions/:id/messages`.

  **The reopen is the resume signal, and it had to be a user action rather than a clock.**
  Consolidation's gate guards "never merely because the process has been up a while". The same
  hazard wears a different costume here: at boot *every* stored session has a gap of days, so a rule
  that asked only about `idle_gap` would compact the entire history store on startup and call each
  one a resume. `ResumeGateInputs::reopened` is that guard, and nothing in `pond-core` can set it
  from a timer — the one production caller sets it from a request a person made. Removing the check
  makes `a_huge_gap_alone_is_not_a_resume` and `no_single_precondition_can_be_dropped` fail with
  *"the gate ran with `reopened` unsatisfied"*.

  **What actually runs on resume today is the rolling-summary refresh, not a "full compaction", and
  the difference is worth stating plainly rather than papering over.** 3.2 says compaction "happens
  *during* the first turn back, while the user waits on a token stream". With hybrid compaction on,
  that is only half true: the in-turn work is the deterministic trimmer, which is cheap and cannot
  be pre-run anyway, because it is a function of the turn being assembled. The expensive, pre-runnable
  half is the summary — and there the phase found a real hole rather than a hypothetical one. The
  idle refresh loop in `pond-server` skips every session whose `updated_at` predates process start
  (its own "never at startup" guard), so a conversation from before the last restart keeps a summary
  that stops where it stopped, and the trimmer splices that stale summary into every turn until four
  new messages accumulate. Refreshing on reopen closes exactly that, and does it while the user is
  reading history rather than waiting on tokens. The large tier's re-summarisation is P2's, and this
  gate will call it when it exists.

  **Too short is the failure that costs something, so both fallbacks lengthen.**
  `RESUME_IDLE_THRESHOLD_SECS` is 30 minutes — 15x the default `summary_idle_secs` and 2x
  consolidation's `INACTIVITY_THRESHOLD_SECS`, which is the point at which the rest of the system
  already considers the household asleep. A threshold that is too *large* only means the user pays
  what they pay today; one that is too small reads a mid-conversation pause as a resume and spends a
  model call between every pair of turns. So `MIN_RESUME_IDLE_SECS = 300` floors whatever is stored
  (a `0` from a hand-edited row must not mean "every reopen"), and `idle_gap_since` returns
  `Duration::ZERO` for a future timestamp rather than an enormous positive gap — clock skew reads as
  "active", never as "stale".

  **Invariant 1 is held structurally, not by promise.** Every input the gate needs, including its two
  database reads, is gathered *inside* the spawned task, so `GET /sessions/:id/messages` returns at
  exactly the speed it did before. The refresh itself races a watcher on `last_user_activity` — the
  same contract the idle loop uses — so a user turn cancels it and nothing is persisted. A
  process-local in-flight set keeps two rapid reopens of one session from queueing two model calls
  ahead of that turn on the serial on-device engine; it is deliberately not a database row, which
  would outlive a `kill -9` and strand the session as permanently compacting.

  **What is deferred, and named so it is not mistaken for done.** Age-weighted retention is P3's and
  is untouched. `PrefixCacheState` is P5's: this phase uses the idle gap as the cache-age proxy the
  design itself offers ("the KV cache is long gone"), and introduces no cache-state type that P5
  would then have to reconcile with. There is no integration test asserting the refresh completed
  before the first token of the next turn (section 7); that needs a live server and belongs with
  `scripts/live-test.sh`.
- **P5** `PrefixCacheState` plumbed from the adapter; `invalidated_by` recorded at each of the six
  known invalidation points; the recompact-when-cold rule.
- **P6** Act on `should_compact` between turns; call `reset_session` on clear and after compaction.
- **P7** `POST /sessions/{id}/compact` plus a desktop control on the existing `ContextCard`.

---

## 5. Invariants

1. Compaction never blocks a turn. Ever.
2. Tool results never orphan from their turn.
3. On the small on-device tier, no compaction path **waits on** a model. Restated 2026-08-06 by P1:
   the original wording ("calls a model") would have switched off the idle rolling summary there, and
   that summary is never awaited by a turn — see the correction under 3.1. What this forbids on the
   two on-device tiers is the large tier's re-summarisation, which compaction does wait for;
   `ModelClass::permits_compaction_model_call()` is the gate, and it is false for `Small` and
   `Medium`.
4. A warm prefix that has served many turns is not perturbed for a marginal token saving.
5. The rolling summary is read, never awaited.
6. Image policy lives in `image_history.rs` and nowhere else.

---

## 6. Deliberate deferrals

- **Compacting `session_messages` itself.** The durable store is pruned by count (500/session,
  `pruning.rs`) and is the authoritative history the UI reads. Summarising it would destroy the
  record to save disk that is not scarce. Compaction stays a property of what the *model* sees.
- **Semantic compaction** (keeping turns by relevance to the current topic rather than recency).
  Interesting, needs an embedding call per turn on the critical path, and interacts badly with
  prefix stability. Revisit after P5 gives us cache-cost measurements.
- **Cross-session compaction.** Belongs to memory consolidation, which already exists.

---

## 7. Verification

- **Unit** — strategy dispatch per model class; the age-weighting priority order; `PrefixCacheState`
  transitions for all six invalidation reasons.
- **Property** — after any compaction, every retained tool result still has its request; the
  estimate never exceeds the profile budget; compaction is idempotent on an unchanged conversation.
- **Measured, Orin and Mac** — TTFT on the turn following a compaction, warm-cache versus cold-cache.
  P5 is only correct if cold-cache recompaction shows no TTFT penalty and warm-cache turns show no
  new re-prefills.
- **Integration** — resume a session after `resume_compaction_idle_secs`, assert compaction completed
  *before* the first token of the next turn.
- **Regression** — a long Jetson conversation produces no `ContextLengthExceeded` and no reactive
  Goose compaction.
- **Manual** — `POST /sessions/{id}/compact` on a saturated session; verify the reported numbers
  match a subsequent `turn_stats`.
