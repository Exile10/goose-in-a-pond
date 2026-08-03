# Personal Agentic Intelligence — programme roadmap

State of the world verified against code on 2026-08-03, and the design programme for the eight
capabilities that turn GIAP from a very good reactive assistant into a personal agentic
intelligence.

Before working on any item in this programme, read [`pai/00-checklist.md`](./pai/00-checklist.md) —
the standing checklist and running scratchpad. It is the entry point; this file is the reference.

All `file:line` references are to this repository unless prefixed `goose/`, in which case they are
to the fork at `jarida-io/Goose:main`. The submodule is **not initialized** in a fresh clone
(`git submodule status` reports `-29ef609c…`), so Goose-side claims here were verified against a
separate checkout; run `git submodule update --init --recursive` to re-verify locally.

---

## 1. Why this programme

GIAP answers well. It selects tools, remembers, forgets on a schedule, sees images, narrows its
tool surface per session, and survives a restart with its conversation intact. Phases A-F of
[context-and-reasoning-roadmap.md](./context-and-reasoning-roadmap.md) landed, and they are the
reason the assistant is usable on an 8 GB Jetson at all.

What it does not do is act like it belongs to *someone*. Four spines are missing, and every one of
the eight requirements below is blocked on one of them.

### 1.1 There is no subject

`Profile` exists (`crates/pond-core/src/user_data/domain/profile.rs:12`) with five fields and a
CRUD repository. Everything downstream of it is unwired:

- `MemoryFragment.profile_id` exists (`user_data/domain/memory.rs:100`) and `sqlite_memory.rs`
  branches on it — but **every production call site passes `None`**:
  `goose_agent.rs:1643,1674,2109`, `pond-mcp-server/src/memory.rs:100,133,372,443`,
  `user_data/services/memory_extraction.rs:84,206`. One household, one memory pool.
- `Session` (`user_data/domain/session.rs:61-77`) has no profile column at all.
- `POST /sessions/{id}/identify-user` writes into `AppState.session_user_bindings`
  (`pond-api/src/lib.rs:266`), an in-memory `HashMap` touched only by its own three handlers
  (`routes.rs:11236,11255,11268`). **Nothing on the chat or prompt path reads it.**
- Only `settings.primary_profile_id` reaches the model (`routes.rs:1191-1205`).
- Speaker identification is a design document (`docs/architecture/data_pipeline.md:502-577`) — no
  port, adapter, table, migration or crate exists.

### 1.2 There is no enforcement

`SecurityPolicy` is a well-designed port with eight scopes and a `Principal` type
(`security/ports/policy.rs:38-111`). Both implementations return `Ok(true)` unconditionally
(`security/services/policy.rs:27-28`, `pond-infra/src/sqlite_security_policy.rs:62-64`), and every
single `.audit()` call site in the repository is inside a `#[cfg(test)]` module.

Alongside that: no general-purpose redaction exists anywhere (the only redaction in the tree is
`pond-infra/src/push_token_log.rs`, which shortens push tokens for logging); there is no encryption
at rest; and `api_key_guardian` / `_gnews` / `_finnhub` / `_coingecko` are plain `Option<String>`
fields on `Settings` (`settings.rs:652-668`) carrying only `#[serde(default)]` — so
`GET /settings`, which serializes the whole struct (`routes.rs:2608`), **returns them**.

### 1.3 There is no initiative

- The event bus is a closed three-variant enum — `Sensor | Camera | Device`
  (`shared/ports/event_bus.rs:29-33`) — with two consumers: the rules engine and the event-log
  bridge.
- Every production notification producer calls `broadcast()`: `routes.rs:542`,
  `pond-mcp-server/src/system.rs:214`, `schedule_executors.rs:107`, `main.rs:2660`. The durable
  offline queue and the FCM relay are only reachable from the *targeted* `send()` path
  (`broadcast_notification_sender.rs:68`), so both are **built, tested and dormant**.
- Unprompted speech does not exist. Every `voice_output.speak()` is downstream of a user utterance
  (`shared/services/chat.rs:933,953,1127,1593,1604`) or an explicit `/tts` request
  (`routes.rs:6395`).

### 1.4 There is no honest accounting

- Four different resolution orders answer "how big is the context window": `goose_agent.rs:907-949`,
  `pond-agent/src/agent.rs:375`, `routes.rs:1674-1685`, and — worst — the live trimmer, which reads
  `std::env::var("GOOSE_CONTEXT_LIMIT")` and **falls back to a hardcoded 8192**
  (`goose_agent.rs:1478-1481`).
- Token estimation is `len/4 + 4` (`turn_trimmer.rs:89`), corrected one turn late.
- `ModelRecord.context_length` exists (`models/domain/model_record.rs:108`) and is written as
  `None` by every production path (`model_service.rs:383,532`, `routes.rs:3174`). No budget code
  reads it.
- `reasoning_content` is parsed by llama.cpp and dropped: the identifier appears exactly once in
  `crates/`, in a comment (`pond-inference/src/provider.rs:422`).
- `AgentStreamEvent::Thinking` has **four consumers and zero producers**
  (`chat.rs:1084`, `routes.rs:1415`, `routes.rs:7180`, `main.rs:6178`).
- `ContextCompactor` — 277 lines of LLM summarisation — is stored on `ChatService` and never read
  (`chat.rs:8,195,197,270,424,425`); nothing anywhere calls `with_context_compactor`.

---

## 2. Sequencing

The eight are interdependent, but not symmetrically. Two are substrate everything personal stands
on, two are accounting every token-hungry feature stands on, two are capability, and two are
product. The product themes land last not because they matter least — they are the entire value —
but because shipping a proactive agent that cannot tell household members apart, or an ingest
pipeline with no redaction, would be worse than shipping neither.

```
PAI-1 Identity ──┬──────────────────────────────────────────┐
                 │                                          │
PAI-2 Guardrails ┴──────────────────────┐                   │
                                        │                   │
PAI-3 Context governor ── PAI-4 Compaction ── PAI-5 Thinking │
                                        │                   │
                                        └── PAI-6 Orchestration ──┬── PAI-7 Proactivity
                                                                  │            │
                                                                  └────────────┴── PAI-8 Ingest
```

| Doc | Workstream | Requirement | Depends on | Status |
|---|---|---|---|---|
| [01](./pai/01-identity-and-profile-boundaries.md) | Identity and profile boundaries | Hard profile boundaries | — | DESIGNED |
| [02](./pai/02-privacy-and-security-guardrails.md) | Privacy and security guardrails | Privacy/security guardrails | 01 | DESIGNED |
| [03](./pai/03-context-governor.md) | Context governor | Large context, used fully | — | DESIGNED |
| [04](./pai/04-smart-compaction.md) | Smart compaction | Smart compaction | 03 | DESIGNED |
| [05](./pai/05-reasoning-and-thinking.md) | Reasoning and thinking | Ability to think | 03, 04 | DESIGNED |
| [06](./pai/06-multi-agent-orchestration.md) | Multi-agent orchestration | Multi-agent orchestration | 01, 02, 03, 04 | DESIGNED |
| [07](./pai/07-proactive-intelligence.md) | Proactive intelligence | Proactive not reactive | 01, 06 | DESIGNED |
| [08](./pai/08-personal-context-streaming.md) | Personal context streaming | Personal context streaming | 01, 02, 07 | DESIGNED |

`DESIGNED` means the document exists and its current-state claims are verified. Phases inside each
document carry their own `LANDED` / `DEFERRED` stamps as work completes, matching the convention in
`context-and-reasoning-roadmap.md`.

### Why this order and not another

- **01 before everything personal.** Memory, drafts, notifications and ingested items all need an
  owner. Retrofitting a `profile_id` through them later means a migration per table plus a
  backfill with no ground truth about who said what.
- **02 before 08.** Ingesting a mailbox into a store with no redaction, no encryption for
  connector tokens, and a policy layer that returns `Ok(true)` would be the single largest privacy
  regression in the product's history.
- **03 before 04, 05 and 06.** Compaction decisions, thinking budgets and per-subagent context
  allocations are all arithmetic on a number that four code paths currently disagree about.
- **04 before 06.** A subagent is a second context window opening on the same device. Without
  cache-age-aware compaction, delegation turns every parent turn into a full re-prefill.
- **06 before 07.** The proactive reasoner is a background agent with its own budget, its own tool
  scope and its own cancellation semantics. That is exactly what 06 builds.
- **07 before 08.** Ingest without a proposer is a database nobody reads. The proactive layer is
  what converts a stream of e-mail and calendar rows into something the household notices.

---

## 3. Cross-cutting invariants

These hold across all eight workstreams. A design that violates one is wrong regardless of how
good it looks in isolation.

1. **The KV prefix is sacred.** The system prefix is the KV prefix, and moving it costs a full
   re-prefill (measured at 3.7 s on the Orin for a 78-character delta, `goose_agent.rs:1000-1011`).
   Anything session-specific or turn-specific rides the user message's `<system-context>` block,
   never the system prompt. This is why `system_prefix` and `extension_appendix` are global by
   design (`context-and-reasoning-roadmap.md`, Phase D1).
2. **Nothing blocks a turn on an LLM call.** Compaction, summarisation, consolidation and
   proactive reasoning all run in idle time and are cancelled by a new turn. A user-visible stall
   at 20 tok/s is a bug, not a trade-off.
3. **Deny by default, widen explicitly.** Every failure path in tool selection already widens to
   all tools; every failure path in *access* must narrow to none. Where those two conflict, access
   wins.
4. **Egress is tracked at the adapter.** Any crate that reaches the network with its own
   `reqwest::Client` calls `record_egress` — copy `traced_send` from
   `pond-adapters-weather/src/lib.rs:15-30`.
5. **The domain owns policy; adapters own mechanism.** Anything that would have to be rewritten if
   Goose were swapped out belongs in `pond-core` behind a port.
6. **Settings fields are dispositioned or the build fails.**
   `every_settings_field_is_dispositioned` (`settings.rs:1630-1795`) must keep passing; new fields
   are consciously classified as UI-wired or headless.
7. **On-device reality bounds ambition.** One GPU, one resident model, ~102 GB/s of memory
   bandwidth. Parallelism that serialises on hardware is complexity without benefit; say so rather
   than shipping it.

---

## 4. Documentation debt corrected alongside this programme

Several existing documents now assert things the code contradicts. Accuracy is what makes these
documents worth keeping, so they are corrected as part of the workstream that touches them.

| Document | Claim | Reality | Fixed in |
|---|---|---|---|
| `architecture/token_tracking.md:28` | "Real token counts are not available from Goose's `AgentEvent` stream" | Migration `0029_message_token_counts` and `TurnStats` landed; they are | PAI-3 |
| `architecture/scheduling.md:38` | `TaskKind` has two variants; seven MCP tools | Three variants (`SensorTrigger`); twelve tools | PAI-7 |
| `api.md:219` | "Token validation is currently a stub" | `SqliteHandshakeAdapter::validate_token` is real | PAI-2 |
| `architecture/model_capabilities.md:14` | `ModelCapabilities` has five fields | Six — `tool_calling` is missing from the doc | PAI-3 |
| `security/ports/policy.rs:6-11` | Describes the unconditional loopback bypass "at lines 206-214" | Removed in #94; now gated behind `POND_DEV_ALLOW_LOOPBACK` | PAI-2 |
| `CLAUDE.md` | "14 `giap-*` extensions" | Correct — `giap_registration.rs` has exactly 14 `register_builtin_extension` calls. My earlier claim of 15 was wrong; re-counted 2026-08-03 | n/a |

---

## 5. What this programme deliberately does not do

- **It does not replace Goose.** `pond-agent` + `pond-inference` remain the quarantined
  independent path. Every design here works through the live `GooseAdapter` and keeps its domain
  logic in `pond-core` so a future swap stays possible.
- **It does not add cloud inference.** `cloud_fallback_enabled` stays off by default and no
  workstream here depends on it. PAI-8 ingests from cloud accounts; it never sends prompts to them.
- **It does not promise full-database encryption.** PAI-2 encrypts the high-sensitivity stores and
  writes down the SQLCipher migration path with its cost, rather than claiming a property the
  build system cannot currently deliver on a Jetson cross-build.
- **It does not promise first-party WhatsApp.** PAI-8 is explicit about which messaging sources
  have a sane official read API and which require a bridge the user runs themselves.
