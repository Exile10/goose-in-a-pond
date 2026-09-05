# Synthetic household personal-context corpus

403 items across a fictional three-person Nairobi household, spanning all eight
`SourceKind`s and all five `ItemKind`s over a 42-day window. Fictional and
committable on purpose: it is a fixture, and a corpus modelled on a real
household could not live in git.

Generated, not hand-maintained:

```bash
python3 scripts/gen-personal-context.py
```

Deterministic — same seed in, same bytes out. Edit the generator, never the
`.jsonl`.

## Why it exists before any connector does

PAI-8's open work was a set of claims nobody had measured. This corpus turns
four of them into numbers without needing a single OS API, a Jetson, or a
working embedder. Run the harness:

```bash
cargo test -p pond-infra --test personal_context_corpus -- --nocapture
```

It drives the corpus through the **production** ingest path — the real
`RuleRedactor` (the only `Redactor` in production) and the real `IngestPipeline`
(the only writer of `context_items`). Only the repository is a mock, which costs
the storage round-trip and buys a hermetic test inside `ci.yml`'s fast-crate
list. Measured 2026-09-04:

| | measured |
|---|---|
| Corpus reachable by the pipeline today | **328 of 403 items; 19% refused** |
| Refused, by kind | `files` 55, `mobile` 11, `chat` 9 |
| Full corpus rendered | 9,172 tokens, 28.0 tokens/item |
| Fits the preamble at the Orin's 16,384 window | **8 items, 2.4%**, 218 tokens |
| Fits at 4,096 | 2 items, 0.6% |
| Labelled queries a recency block can answer | **0 of 22** |
| Redactor false positives over 9 bait items | **0** |

### The four findings

1. **A recency-ordered block answers none of the questions.** 0 of 22 answerable
   queries. The eight items that fit at 16,384 are two hall-PIR motion rows, a
   bank statement, a rate-card notice and four ordinary calendar entries — not
   one of them relevant to anything a person would ask. This is the PAI-3
   retrieval argument with a number on it, and the number is zero.

2. **Sensor rows crowd out everything else.** They are cheap per item and carry
   the least, so they win a recency contest and spend a quarter of the block.
   Whether `Sensor` should compete in the same budget as `Mail` is now a
   question with evidence behind it.

3. **The `ApiKey` rule has no reachable caller through context ingest.** A
   credential checked into a `.env` on a laptop is the case that rule exists
   for, and `Files` is `AwaitingReadConnector`, so the pipeline refuses it. The
   corpus keeps that item (`env-file-doc`) where it realistically lives and the
   harness reports it as BLOCKED rather than failing. A second, reachable
   key (`key-in-mail`) exercises the replacement path meanwhile.

4. **Three of 25 queries are unanswerable at any budget**, because every item
   that would answer them is on a gated source. Not a retrieval failure; a
   connector gap, and a different fix.

Two behaviours the harness now asserts rather than describes:

- At `INGEST_REDACTION_LEVEL` (`Secrets`) only `PaymentCard`, `Iban` and
  `ApiKey` are **replaced**. Email, phone and postcode are detected, reported in
  `findings`, and **left in the stored text**. 3 spans replaced, 14 contact
  details reported and left in place.
- `ContextItem::from_parts` redacts title, body **and every participant**, so
  any mail item with an address in `participants` carries an `email` finding
  whatever its body says.

## Files

| file | what it is |
|---|---|
| `household.jsonl` | one `RawItem` per line, plus the `source_id` that routes it |
| `sources.json` | the 13 `ContextSource` definitions the items route to |
| `expectations.jsonl` | what the redactor must find per item, and what it must not |
| `queries.jsonl` | 25 household questions with the items that answer them |
| `manifest.json` | seed, anchored `corpus_now`, counts by kind |

### `household.jsonl` is the ingest route's payload schema

Each line is exactly a `pond_core::context::ingest::RawItem` plus a `source_id`.
Nothing else. That is deliberate: `POST /api/v1/context/ingest` — named by
`SourceAvailability::AwaitingIngestRoute` and the thing a host-device companion
would push to — does not exist yet, and authoring the corpus designs its payload
before the route is written.

Note what is **absent**: no line carries a `profile_id`. The owner is resolved
from the source, per the invariant `raw_item_does_not_name_its_own_owner`
enforces in `ingest.rs`. A corpus that named its own owner would encode the
wrong shape and the harness asserts it does not.

`corpus_now` is anchored (2026-09-04T18:30Z), not the wall clock. Every recency
score would drift with the calendar otherwise, and a fixture whose measurements
change daily measures nothing.

## What it does not prove

- **No storage round-trip.** The repository is a mock, so nothing here exercises
  `SqliteContextRepository`, the `UNIQUE (source_id, external_id)` constraint,
  the vector index, or retention. A follow-up harness against a migrated temp
  database would.
- **No embeddings.** Every `similarity` is `None`, which is not a shortcut: it
  is the state of the deployed pond, where the only production
  `EmbeddingProvider` does not initialise on the Orin. Recall@k over
  `queries.jsonl` is the comparison to run once that works, and it has to beat
  0 of 22.
- **No token count from a tokenizer.** `estimated_tokens` is the chars/4
  heuristic the memory loop uses. Good enough to compare against a budget set
  with the same heuristic; not a number to quote as prefill cost.
- **The prose is invented.** Retrieval quality against real mail is a different
  measurement, and a harder one.
