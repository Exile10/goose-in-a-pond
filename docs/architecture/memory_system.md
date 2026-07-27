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
number of memories to be useful). The shipped pipeline is three-stage and
adversarial (`crates/pond-server/src/three_stage_consolidator.rs`), not the
single-pass merge this document originally described:

1. **Proposer** reads the scoreable memories and proposes actions
   (`Merge` / `Prune` / `Split` / `Recategorize`).
2. **Adversary** challenges each proposal, arguing against destructive or
   lossy changes.
3. **Judge** rules on each exchange; only accepted actions are applied.

Correction-safety guards are unconditional: a `Correction` memory is never
pruned, a merge involving one is forced to `segment=Correction`, and `corrects`
metadata propagates to the merged fragment. Every lifecycle change is written
to `memory_events`, and each run is recorded in `consolidation_runs`.

**Trigger and control**: a background loop starts a run after 15 minutes of
user inactivity; any chat turn cancels an in-flight run (checked between
stages — a stage already inside an LLM call finishes first).
`POST /api/v1/memory/consolidate` streams a run's events over SSE for the
desktop's consolidation banner, and `.../consolidate/stop` cancels it.

Known gaps (tracked in `docs/architecture/context-and-reasoning-roadmap.md`,
Phase E): the trigger fires ~15 min after a boot with no user activity;
`memory_consolidation_mode`, `_interval_hours`, and `_batch_size` are persisted
and UI-exposed but not read by the runtime (mode is always adversarial, and
there is no batch cap); toggling the enable flag needs a server restart.

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
