# Memory System (Enhanced)

Segment-based memory with importance scoring, exponential decay, automatic extraction from conversations, and periodic consolidation. Inspired by [boop-agent](https://github.com/raroque/boop-agent).

## Architecture

```
  Conversation Turn
        │
        ▼
  ┌─────────────┐    tokio::spawn (async, never blocks SSE)
  │  Memory     │───────────────────────────────────────────┐
  │  Extractor  │  LLM: "Extract durable facts as JSON"     │
  └─────────────┘                                           │
        │ Vec<ExtractedFact>                                │
        ▼                                                   │
  ┌─────────────┐    dedup against recent 20 memories       │
  │  Extraction │                                           │
  │  Service    │    rate limited (configurable interval)    │
  └──────┬──────┘                                           │
         │ MemoryFragment::from_extraction()                │
         ▼                                                  │
  ┌─────────────┐                                           │
  │  SQLite     │◄──────────────────────────────────────────┘
  │  memory_    │    segment, importance, tier, decay_rate,
  │  fragments  │    access_count, last_accessed_at, lifecycle
  └──────┬──────┘
         │ every 6 hours
         ▼
  ┌─────────────┐    effective_score = importance * exp(-λt) * reinforcement
  │  Cleanup    │    < 0.05 → prune  |  < 0.15 → archive
  │  Service    │    permanent tier exempt
  └─────────────┘
         │ after 15 min of user inactivity (opt-in, cancelled by activity)
         ▼
  ┌─────────────┐    three-stage adversarial pipeline:
  │  Consolidat.│    Proposer -> Adversary -> Judge (3 LLM calls)
  │  Service    │    merge / prune / split / recategorize
  └─────────────┘
```

## Memory Segments

| Segment | Default Importance | Default Tier | Decay Rate | Description |
|---------|-------------------|-------------|------------|-------------|
| Identity | 0.8 | Permanent | 0.00 | Name, role, location — core facts |
| Correction | 0.9 | Long | 0.01 | User correcting the assistant |
| Preference | 0.7 | Long | 0.01 | Likes, dislikes, habits |
| Relationship | 0.7 | Long | 0.01 | People the user knows |
| Project | 0.6 | Long | 0.01 | Ongoing work, goals |
| Knowledge | 0.5 | Long | 0.01 | Factual knowledge |
| Context | 0.3 | Short | 0.10 | Transient situation (expires fast) |

## Memory Tiers

| Tier | Decay Rate | Behavior |
|------|-----------|----------|
| Permanent | 0.00 | Never decays, never pruned |
| Long | 0.01 | Retained for weeks/months |
| Short | 0.10 | Expires within days |

## Lifecycle States

- **Active** — included in searches and agent context
- **Archived** — hidden from recall, retained in DB
- **Merged** — consolidated into another memory, `superseded_by` links to replacement

## Decay Formula

```
effective_score = importance * exp(-decay_rate * days_since_access) * (1 + ln(access_count + 1) * 0.1)
```

- `days_since_access`: days since `last_accessed_at` (or `created_at` if never accessed)
- `access_count`: incremented each time the memory is recalled or injected into a prompt
- Reinforcement term `(1 + ln(N+1) * 0.1)`: frequently recalled memories resist decay

**Thresholds** (configurable via settings):
- Below `memory_prune_threshold` (default 0.05) → pruned
- Below `memory_archive_threshold` (default 0.15) → archived
- Permanent tier → exempt from decay

## Background Extraction

After each chat response is persisted, a background task extracts durable facts:

1. **Rate limit**: skips if last extraction was < `memory_extraction_interval_secs` (default 10s) ago
2. **Minimum length**: skips if both user message and response are < 15 chars
3. **LLM call**: compact prompt (~150 tokens) asks for JSON array of facts
4. **Dedup**: substring match against 20 most recent memories
5. **Storage**: each fact becomes a `MemoryFragment::from_extraction()` with segment/importance/tier

The extraction prompt is designed for 3B models:
```
Extract durable facts from this conversation. Output JSON array only.
Each: {"fact":"...","segment":"identity|preference|...","importance":0.0-1.0}
Skip: greetings, small talk. Max 3 facts.
```

## Auto-Classification

When saving via MCP tool without explicit segment, `auto_classify_segment()` uses keyword heuristics:
- "I prefer/like/hate" → Preference
- "My name is/I am a" → Identity
- "Actually/No, that's wrong" → Correction
- "My wife/friend/boss" → Relationship
- "Working on/my project" → Project
- "Right now/currently" → Context
- Default → Knowledge

## Consolidation

Opt-in (`memory_consolidation_enabled`, default false — it needs a meaningful
number of memories to be useful). Two modes, selected by
`memory_consolidation_mode`:

- **`single`** (default) — one LLM call
  (`crates/pond-server/src/llm_memory_consolidator.rs`). Every proposal is
  applied. On a 3B on-device model this is the sane choice for a background
  chore; the audit trail records each action as accepted *without* review so it
  never implies a scrutiny that did not happen.
- **`adversarial`** — three sequential LLM calls
  (`crates/pond-server/src/three_stage_consolidator.rs`), cancellable between
  stages:
  1. **Proposer** reads the batch and proposes actions
     (`Merge` / `Prune` / `Split` / `Recategorize`).
  2. **Adversary** challenges each proposal, arguing against destructive or
     lossy changes.
  3. **Judge** rules on each exchange; only accepted actions are applied.

Both modes share one apply path,
`pond_core::user_data::services::memory_consolidation::apply_actions`, so the
correction-safety guards cannot drift between them. Those guards are
unconditional: a `Correction` memory is never pruned, a merge involving one is
forced to `segment=Correction`, `corrects` metadata propagates to the merged
fragment (and, on a split, to the first `Correction`-segment entry), and a
replacement that fails to insert aborts before its sources are marked
superseded. Every lifecycle change is written to `memory_events`, and each run
is recorded in `consolidation_runs` tagged with the mode that produced it.

**Batch cap**: `select_batch` bounds each run at
`memory_consolidation_batch_size` eligible (segmented) memories, **oldest
first** — duplicates cluster in time, so a contiguous window is the ordering
most likely to hold both halves of a duplicate pair. Whatever does not fit is
logged as `deferred`, never silently dropped. Known limitation: the window does
not rotate, so on a store larger than the batch, newer memories are not reached
until the oldest ones are acted on. Advancing a persisted cursor would need a
schema column and is deliberately future work.

**Trigger and control**: the scheduler policy lives in
`pond_core::user_data::services::consolidation_schedule` and is *at most one run
per `memory_consolidation_interval_hours`, and only after 15 minutes of
inactivity following real user activity in this process lifetime*. A freshly
booted server that nobody has talked to never consolidates, however long it
idles. The enable toggle, mode, interval, and batch size are all re-read from
the settings DB on every tick, so changing them in Settings takes effect without
a restart.

Activity is a **two-source** signal, because the terminal voice loop runs in a
separate OS process (`pond-server chat --json-events`) and cannot reach the
server's in-memory state: the in-process `last_user_activity` clock (reset by
every route through `AppState::note_user_activity`) *and* the newest
`sessions.updated_at` in `pond_system.db`, which the voice child bumps through
`ChatService` on every turn it persists. Either source going active both holds a
run off and cancels one already in flight (adversarial stages check the token
between calls; a stage already inside an LLM call finishes first).

`POST /api/v1/memory/consolidate` streams a run's events over SSE for the
desktop's consolidation banner, and `.../consolidate/stop` cancels it.

Known gaps (tracked in `docs/architecture/context-and-reasoning-roadmap.md`):

- The batch window does not rotate (above).
- The 15-minute inactivity threshold is a compile-time constant
  (`INACTIVITY_THRESHOLD_SECS`) rather than a setting.
- Cross-process cancellation is poll-based, not pushed: the in-process clock is
  sampled every 500 ms and the DB every 2 s, so an out-of-process voice turn can
  wait up to about two seconds behind an in-flight LLM call.
- An attempt consumes the interval budget even when cancelled or skipped for too
  few memories. On a device the user touches every evening this can starve
  consolidation for long stretches. The tradeoff is deliberate — retrying at the
  next idle window is what made the old loop churn — but it is the part of the
  policy most likely to want revisiting after Jetson soak time.

## MCP Tools

| Tool | Description |
|------|-------------|
| `save_memory` | Save with optional segment, importance, tier. Auto-classifies if segment omitted. |
| `recall_memories` | Vector search when a query is given and an embedding provider is available (keyword fallback otherwise); recency-ordered without a query. Returns segment + importance metadata. Records access. |
| `forget_memory` | Delete by ID or exact content match. |

## REST API

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/memories` | List recent memories (up to 50) |
| POST | `/api/v1/memories` | Save with content, source, tags |
| DELETE | `/api/v1/memories/{id}` | Delete by ID |

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `agent_memory_inject` | false | Inject memories into system prompt each turn |
| `agent_memory_limit` | 5 | Max memories to inject |
| `memory_extraction_enabled` | false | Enable background extraction |
| `memory_cleanup_enabled` | false | Enable periodic decay cleanup |
| `memory_consolidation_enabled` | false | Enable periodic merge/prune |
| `memory_prune_threshold` | 0.05 | Score below which memories are pruned |
| `memory_archive_threshold` | 0.15 | Score below which memories are archived |
| `memory_cleanup_interval_hours` | 6 | Hours between cleanup runs |
| `memory_consolidation_interval_hours` | 24 | Hours between consolidation runs |
| `memory_consolidation_batch_size` | 20 | Max memories per consolidation batch |
| `memory_extraction_max_facts` | 3 | Max facts extracted per turn |
| `memory_extraction_interval_secs` | 10 | Minimum seconds between extractions |

## Database Schema (migration 0015)

```sql
ALTER TABLE memory_fragments ADD COLUMN segment TEXT;
ALTER TABLE memory_fragments ADD COLUMN importance REAL;
ALTER TABLE memory_fragments ADD COLUMN tier TEXT DEFAULT 'long';
ALTER TABLE memory_fragments ADD COLUMN decay_rate REAL;
ALTER TABLE memory_fragments ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memory_fragments ADD COLUMN last_accessed_at TEXT;
ALTER TABLE memory_fragments ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'active';
ALTER TABLE memory_fragments ADD COLUMN superseded_by TEXT;
```

All columns are optional/defaulted — backward compatible with existing memories.
