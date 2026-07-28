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
  ┌─────────────┐    quality gate, then dedup against the   │
  │  Extraction │    50 most recent memories                │
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
3. **LLM call**: compact prompt asks for `{"facts":[…]}` (see `crates/pond-server/src/llm_memory_extractor.rs`)
4. **Normalise**: collapse whitespace, drop an invented label prefix ("Active Project: …")
5. **Quality gate**: reject content that cannot survive outside its conversation (below)
6. **Dedup**: substring *and* token overlap against the 50 most recent memories, plus
   the facts already stored by this same run — corrections exempt (below)
7. **Reclassify**: a `Project` fact that is really a captured request is filed as `Context`
8. **Storage**: each fact becomes a `MemoryFragment::from_extraction()` with segment/importance/tier

The extraction prompt is the entire per-turn prefill of a job that runs after *every* turn,
on the same single-slot local model that is serving chat, so its length is felt as latency
on the next user message, not just as context. It is held to ~400 tokens (1589 chars) and a
unit test fails the build above `EXTRACTION_PROMPT_CEILING` (1800 chars) — it had reached
2466 by accretion, roughly doubling that background prefill.

`MemoryExtractionService` is the **write gate**: steps 4–7 run there, in `pond-core`, for
every extractor adapter. `LlmMemoryExtractor` applies the same normalise/gate/dedup checks
while parsing, but only so junk cannot consume the `memory_extraction_max_facts` budget
ahead of good facts — the core service does not trust it.

### Fact quality gate

A stored memory is injected into the **assistant's** context many turns later, with none
of the conversation that produced it. Content that only made sense inside that
conversation does not merely waste tokens, it misleads. `fact_defect()` in
`pond-core/src/user_data/domain/memory.rs` rejects three classes outright:

| Defect | Example (real rows from a production store) | Rule |
|--------|---------------------------------------------|------|
| `UnresolvedReference` | "The user's mother lives in the latter city." | "the latter"/"the former" with no named referent, a non-expletive "there", a deictic like "that place", or a sentence opening on a bare pronoun |
| `FirstPerson` | "My mom's name is Florence…" | any of "I / me / my / we / our", including contractions ("I'm", "I've", "We're") — in the assistant's context "my mother" reads as the *assistant's* mother |
| `TooShort` | "Tea." | fewer than `MIN_FACT_CONTENT_LEN` (8) characters |

The gate is deliberately hard to trip. A rejected fact is *lost*, so every rule is anchored
to a token pattern a well-formed third-person sentence cannot produce, and each has
explicit escapes: "The user's therapist…" is not "there", "lives in the US" is not "us",
"Type I diabetes" is not "I", "There is a spare key…" is the expletive, "grew up in the
former Yugoslavia" names its referent, and "moved from Nairobi to Kisumu and prefers the
latter" resolves inside its own sentence. Defective facts are **not repaired**: conjugating
"I like" into "The user likes" or inventing the referent of "the latter" is the judgement
we do not have at write time, and a wrong repair outlives the conversation that could have
corrected it. The extraction prompt carries the same rules with a worked example, so
compliance is the common case and the gate is the backstop.

Two rules here were tightened and then deliberately loosened again, because each
refinement destroyed more real facts than the imprecision it removed:

- **"mine" is not a first-person marker at all.** It is a common noun ("a coal mine") far
  more often than a predicate pronoun ("that laptop is mine"), and no rule separated the two
  cleanly: checking the previous token alone let one adjective discard "The user works in a
  **coal** mine near Kakamega."; walking back past premodifiers to find a determiner then
  admitted "a friend **of** mine works at Jarida" as third person. Both directions were
  wrong, so the disambiguation is gone and "mine" is simply not checked. The cost is that
  "That laptop is mine now." is stored — one imprecise row, against a rule that was
  destroying correct ones.
- **An antecedent only has to precede its anaphor.** The old test asked whether *any* token
  after the first was capitalised, which a name, city, month or weekday satisfies — i.e.
  most real facts — so the rule almost never fired. Antecedents are now counted
  **positionally**, before the deictic, and nothing else about them is inspected. Requiring
  a *place* antecedent for "there" and "that city" (a proper noun after a locative
  preposition) looked more precise and destroyed every fact whose place arrives through a
  copula: "The user's home town is Kisumu and his parents still live there." "the
  latter"/"the former" still need `CONTRASTIVE_ANTECEDENTS` (2) because they *select between
  two* candidates, which is what catches the original production row ("The user's mother
  Florence lives in the latter city" names a person, not a city). The residual imprecision
  is that a single earlier name of any kind resolves "that place" — "The user's brother
  Peter enjoyed that place in March." is stored, vaguely.

### Captured requests are not projects

"Set a reminder to water the plants every evening at 6 PM" is a task the assistant already
carried out, not an ongoing commitment — but it was landing in the `Project` segment
(`Long` tier, decay 0.01) and staying there forever. A `Project` fact that
`is_captured_request()` recognises is reclassified to `Context`: importance 0.3, `Short`
tier, so it still informs the next few turns and then archives itself in about a week. It
is reclassified rather than dropped because the short-horizon information is genuinely
useful; only its permanence was wrong. The rule fires **only** for `Project` — an
imperative in a `Preference` ("Use one word for goodbyes") is a real standing instruction
and must keep its tier.

A bare imperative opener is **grammar, not transience**, and keying on it alone demoted
exactly the long-lived commitments the `Project` segment exists for: "Build a treehouse for
the children this summer", "Write a novel about beekeeping", "Run the Nairobi marathon in
October", "Design the new logo for Jarida". Two signals are therefore required — an
imperative opener from `TASK_VERBS` **and** a named assistant artifact in the object
(`ASSISTANT_ARTIFACT_NOUNS`: reminder, function, script, email, summary …, things that get
produced and finished).

The **object is the whole signal**; the verb never is. An "assistant-only verb" list was
tried as a second single-signal arm and it demoted "Convert the garage into a workshop this
year" and "Install the solar panels on the roof before the rains" — year-long undertakings
that happen to open on a verb an assistant also answers to. There is no verb that means
"this is assistant work" independent of what it acts on, so the list is gone. The cost is
under-firing: "Translate the poem into Swahili." and "Explain how the decay formula works."
are captured requests and are no longer demoted. That is the intended direction — the rule
is calibrated to **under-demote**, because missing a captured request leaves a stale
`Project` row that consolidation can retire, while demoting a real project drops it to
`Short` tier and it decays out of the store inside a week.

### Dedup without embeddings

Semantic dedup (cosine ≥ `SEMANTIC_DEDUP_THRESHOLD`) is inert when
`embedding_provider = "none"`: nothing is embedded, so nothing is compared. The
non-semantic path in `memory_relevance.rs` therefore has to stand alone, and substring
containment is not enough — "The user's mother's name is Florence." and "My mom's name is
Florence…" share no substring at all. `is_duplicate_content()` layers:

1. **Polarity** — opposite polarity is never a duplicate ("is happy" / "is not happy"),
   checked first because a token measure is negation-blind.
2. **Argument order** — a token measure is *also* order-blind, and "prefers dark mode over
   light mode" against "prefers light mode over dark mode" reduces to the **identical**
   token set: Jaccard 1.00, containment 1.00. `is_argument_swap()` catches the reversal by
   splitting both texts at a shared `ORDER_SENSITIVE_MARKERS` connective (over, than, to,
   from, instead, rather, before, after, versus …) and checking whether its arguments
   crossed in both directions. "from" is there for the reversed journey — "moved **to**
   Nairobi from Kisumu" against "moved **to** Kisumu from Nairobi", which splitting at "to"
   cannot see because the crossing is entirely on one side of it. Copulas are deliberately
   absent from that list — "Florence is the user's mother" and "The user's mother is
   Florence" are one fact, so a general order-sensitivity test (Kendall tau, bigram
   equality) would break them; only these connectives make word order meaning-bearing.
3. **Substring containment**, case-insensitive — the cheap exact-restatement case.
4. **Token overlap** — content words after possessive stripping, crude singularisation,
   kinship aliasing (mom → mother), and a dedup-specific stopword list that also drops
   "user" (every third-person fact has it, so it carries no signal). Duplicate requires
   Jaccard ≥ 0.45 **and** containment ≥ 0.8 over the shorter side.

Both floors are required. Jaccard alone misses a short restatement of a long fact;
containment alone merges "prefers dark mode" with "prefers dark roast coffee". The pair is
tuned against real duplicate and non-duplicate rows (`memory_relevance.rs` tests) —
notably, "mother lives in Kisumu" vs "mother lives in Nairobi" clears Jaccard but fails
containment, which is the right answer: contradictions must both be stored and left to
consolidation. The measure is only trusted when both sides have ≥ 3 content words.

Comparison window is `DEDUP_RECENT_WINDOW` (50, up from 20). These strings never reach an
LLM prompt, so the window is sized for recall, not tokens.

### Corrections are exempt from dedup

A correction restates the claim it overturns, in almost the same words — which is exactly
what both the lexical measure and the ≥ 0.92 cosine pass score as a duplicate. Dropping it
leaves the **stale** row standing, so the store ends up asserting the thing the user just
took the trouble to deny. That is the worst outcome the memory system can produce, and it
is silent.

So a fact whose segment is `Correction` **or** that carries a `corrects` field (the same
disjunction as `MemoryFragment::is_correction()`, since a small model routinely sets one
without the other) skips both dedup passes and is written. Both halves of the pair then
sit in the store and consolidation supersedes the old row — that path is the only one that
knows which of the two won, and it already refuses to prune a `Correction`. The exemption
is enforced in `MemoryExtractionService` (the write gate) and mirrored in
`LlmMemoryExtractor`'s parse-time prefilter, where the segment and `corrects` field are now
read *before* the dedup check so the exemption can see them.

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
