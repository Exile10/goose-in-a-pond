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

GIAP also disables Goose's competing machinery on this path: `GOOSE_AUTO_COMPACT_THRESHOLD = "1.0"`
and `GOOSE_TOOL_PAIR_SUMMARIZATION = "false"` for local/gguf (`goose_agent.rs:120-160`).

### 1.2 Compaction is model-tier-aware and nothing else

`CompactionProfile::from_context_window` varies *budgets* by window size. It does not vary
*strategy*, and nothing anywhere considers how old a turn is or what state the KV cache is in.

### 1.3 `ContextCompactor` is dead code

277 lines of LLM summarisation (`context/context_compactor.rs`): `needs_compaction`, `compact`,
`summarise`, an `[Earlier conversation summary]` splice, and a `trim_to_budget` fallback on LLM
error. It is stored on `ChatService` as `compactor: Option<ContextCompactor>` (`chat.rs:197`),
initialised `None` (`:270`), settable via `with_context_compactor` (`:424-425`) — and **never
read**. No caller of `with_context_compactor` exists anywhere in `crates/`.

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
| Small on-device (`local`/`gguf`, ≤ 12K window) | Deterministic trim only. No LLM in the compaction path — a summarisation stall at 20 tok/s is a user-visible hang, which is precisely why hybrid compaction exists. |
| Medium (32K, Ollama/llamafile) | Deterministic trim + idle rolling summary (today's behaviour). |
| Large / HTTP (≥ 64K) | Deterministic trim + idle summary + **LLM re-summarisation of the summary itself** when it grows stale. Here the summarisation call is cheap and off the critical path. |

The large-model tier is where the dead `ContextCompactor` is **revived rather than deleted**. It is
correct code solving a problem the on-device tier does not have. It gets wired behind the governor,
given an `LlmProvider`, and gated on model class — which is also the answer to why it was never
wired: nobody had a tier where it was safe.

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

- **P1** `ModelClass` derived from the governor; strategy dispatch; today's behaviour preserved for
  the small and medium tiers.
- **P2** Revive `ContextCompactor` for the large tier — wire it, give it a provider, gate it on class,
  and add the first test that actually executes it.
- **P3** Age-weighted retention priority in the trimmer, with `compaction_verbatim_days` as a
  headless setting.
- **P4** Compact-on-resume, reusing the `should_run` gate shape from `consolidation_schedule.rs`.
- **P5** `PrefixCacheState` plumbed from the adapter; `invalidated_by` recorded at each of the six
  known invalidation points; the recompact-when-cold rule.
- **P6** Act on `should_compact` between turns; call `reset_session` on clear and after compaction.
- **P7** `POST /sessions/{id}/compact` plus a desktop control on the existing `ContextCard`.

---

## 5. Invariants

1. Compaction never blocks a turn. Ever.
2. Tool results never orphan from their turn.
3. On the small on-device tier, no compaction path calls a model.
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
