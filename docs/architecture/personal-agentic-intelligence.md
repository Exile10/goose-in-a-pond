# Personal Agentic Intelligence — programme roadmap

State of the world verified against code on 2026-08-03 and re-audited 2026-08-04 (section 1.4 and
the status table below carry the corrections), and the design programme for the eight
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

`Profile` exists (`crates/pond-core/src/user_data/domain/profile.rs`) with six fields and a
CRUD repository. Everything downstream of it is unwired:

- `MemoryFragment.profile_id` exists (`user_data/domain/memory.rs`) and `sqlite_memory.rs` branches
  on it — but **every production call site passed `None`**: `goose_agent.rs`,
  `pond-mcp-server/src/memory.rs`, `user_data/services/memory_extraction.rs`. One household, one
  memory pool.
  **PARTLY FIXED 2026-08-04 by PAI-1 P1.** The five search methods take `&ProfileScope` now, so
  "everything" is a deliberate, greppable act. Every call site still says `Household`; narrowing
  them is P3 and P4.
- ~~`Session` has no profile column at all.~~ **Wrong, and the truth was worse.** The column has
  existed since migration `0003`, unwritten and unread for thirty-four migrations — a dead column,
  the same failure as `memory.profile_id` and the identification map below.
  **FIXED 2026-08-04 by PAI-1 P2**, which also had to add a delete trigger: the column's untested
  `NO ACTION` foreign key would have started failing member deletion the moment anything wrote it.
- `POST /sessions/{id}/identify-user` wrote into `AppState.session_user_bindings`, an in-memory
  `HashMap` touched only by its own three handlers. **Nothing on the chat or prompt path read it.**
  **FIXED 2026-08-04 by PAI-1 P2** — the map is deleted and the three handlers read and write the
  session row, so a binding now survives a restart. Still nothing on the chat path consults it;
  that is P3.
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
fields on `Settings` carrying only `#[serde(default)]` — so `GET /settings`, which serializes the
whole struct, **returns them**.

**And it used to return them to anybody.** Found during the 2026-08-04 audit, not in the original
pass: `is_public_route` matched on the request path alone — `auth_middleware` never passed it the
method — while its entries were commented as though method-scoped ("PUT /settings is public so
onboarding can save", "POST — create profile during onboarding").
`public_routes.merge(protected_routes)` then puts everything behind that one check, so the
`protected_routes` label decided nothing. The consequences were `GET /settings` (every API key) and
`DELETE /profiles/{id}` reachable **with no token at all**.

**Fixed 2026-08-05** — [PAI-2](./pai/02-privacy-and-security-guardrails.md) P0. The allowlist is now
a `(Method, path)` table with segment-wise wildcard matching, and three compile-time guards fail the
build if it and the router ever disagree. The secrets-on-`Settings` half of this section is
untouched and remains PAI-2 P2: the fix stops `GET /settings` being *reachable*, not the keys being
*on the struct*.

> **Three more of this section's claims went false on 2026-08-05 and were not struck through at the
> time. Corrected while landing PAI-2 P1**, because a roadmap that overstates the danger is read
> the same way as one that understates it — sceptically, and then not at all.
>
> - **"Every `.audit()` call site is inside `#[cfg(test)]`" is no longer true.** There are two
>   production call sites: `evaluate_identity_assertion` in `pond-api/src/routes.rs`, and the draft
>   decision gate in `pond-mcp-server/src/draft.rs`. `SecurityPolicy::allow` does still return
>   `Ok(true)` unconditionally — both gates decide with pure rules in
>   `security/ports/policy.rs` and use the port for the audit trail, which is what `audit` mode is.
> - **"There is no encryption at rest" is half false.** `secrets.json` is an XChaCha20-Poly1305
>   envelope since PAI-2 P4. Both SQLite databases are still plaintext, and that is a recorded
>   deferral rather than an oversight.
> - **The four `api_key_*` fields are off `Settings`** since PAI-2 P2 and live in `SecretRepository`;
>   a build-breaking guard rejects any new secret-shaped field. `GET /settings` no longer serialises
>   a key because there is no longer a key on the struct.

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

- ~~Four different resolution orders answer "how big is the context window", worst of them the live
  trimmer reading `std::env::var("GOOSE_CONTEXT_LIMIT")` with a hardcoded 8192 fallback.~~
  **FIXED 2026-08-04 by PAI-3 P1.** `ContextGovernor` owns the precedence, all four sites are
  repointed, and `no_budget_path_reads_the_context_limit_from_the_environment` keeps the env read
  from coming back.
- ~~Token estimation is `len/4 + 4`, corrected one turn late.~~ **PARTLY FIXED by PAI-3 P2.** A
  `TokenCounter` port now carries it, with a tiktoken-backed adapter on the live path and chars/4 as
  the declared fallback. The "corrected one turn late" half stands and is now load-bearing on
  purpose: no reachable counter is *exact* for a GGUF model.
- ~~`ModelRecord.context_length` exists but nothing feeds it.~~ **PARTLY FIXED 2026-08-06 by PAI-3
  P3.** The earlier correction — `gguf_record()` writes a real value, so it is not `None`
  everywhere — was true and one question short: `llamafile_record()` and `ollama_entry_to_record()`
  wrote `None`, and Ollama is the provider class rung 3 was designed for. Both now populate it
  (Ollama from `/api/show`'s `model_info`), the Gemma 4 rows were corrected from a copy-pasted 8192
  to the declared 131072, and rung 3 clamps a catalog value by the local ceiling because a declared
  maximum is not an allocation. ~~**`WindowSource::CatalogRecord` is still unreachable in
  production**~~ — **FIXED 2026-08-06 by PAI-3 P3b.** Both live `ContextInputs` sites now supply it:
  `routes.rs` through `state.model_repo`, and `GooseAdapter` through a `model_repo` threaded in for
  this (it had none, and `resolve_window` was static). `ModelStatusEntry.context_length` carries it
  to the Models UI, whose `CapabilityBadges` now prefers it over the frontend's own name heuristic.
  P3b also narrowed the rung: a catalog value is bounded by a lower `context_window_override`, and
  reports `WindowSource::Override` when that binds — otherwise populating the catalog would have
  overruled the hand-tuned KV cache the override exists for. Only the quarantined
  `pond-agent/src/agent.rs` still passes `None`.
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
| [01](./pai/01-identity-and-profile-boundaries.md) | Identity and profile boundaries | Hard profile boundaries | — | **COMPLETE — P1-P8 LANDED** |
| [02](./pai/02-privacy-and-security-guardrails.md) | Privacy and security guardrails | Privacy/security guardrails | 01 | **P0-P6a, P7, P8a LANDED** (P3, P5 and P6b partial). **P6a LANDED 2026-08-06** — five of P5's six ungated senders gated, plus a sixth the guard could not see: the ~100 MB ONNX Runtime fetch, which egresses via a `curl` subprocess and so was structurally invisible to a detector that finds senders by looking for `reqwest`. `UNGATED_SENDERS` 6 → 1, `MAX_UNGATED` 6 → 1. Gating it exposed that `set_network_mode` had exactly ONE call site, so `network_mode = "offline"` was a silent no-op on `pond chat` (the terminal voice loop), `pond setup`, and — found by the synthesis pass, where it was failing OPEN — `pond models`. **P6b PARTIALLY LANDED 2026-08-06** (`9dd129bf`) — **one of its three parts**: `routes.rs`'s egress sites are gated and `run_agent_cmd` installs the mode, so `UNGATED_SENDERS` is now EMPTY and `MAX_UNGATED` is 0. This cell said a flat `LANDED 2026-08-07` for two days and was wrong twice — the commit is dated 2026-08-06, and PAI-2's section 4 splits P6b into three parts, of which two remain PAI-8-blocked: the draft gate for outbound connector actions, and P3's third redaction chokepoint. The latter is blocked because there is **no call site to wire** until PAI-8 builds the first connector that sends a body outward, which makes it a two-way dependency rather than a queue. The empty list is kept deliberately — an empty list with a zero cap is the positive claim "there is no known ungated sender", re-proved by the partition test on every run, and deleting it would let the next unclassified sender be classified by adding an entry rather than by gating the call. It remains a statement about what the guard can SEE, not a proof that nothing can egress. **P8a LANDED 2026-08-07** (`6f25e405`): `PolicyDecision::verdict()` returns `allow`/`would_deny`/`deny` as a field rather than a log-line shape, so the enforce flip has a number to read instead of a grep to run. **P8b remains BLOCKED** on a release's worth of that telemetry |
| [03](./pai/03-context-governor.md) | Context governor | Large context, used fully | — | **P1-P4, P6 LANDED** (P3 completed by P3b 2026-08-06); **P5 code landed 2026-08-06, awaiting the on-device TTFT measurement that decides it** |
| [04](./pai/04-smart-compaction.md) | Smart compaction | Smart compaction | 03 | **P1, P2, P3, P4, P6 LANDED 2026-08-06** (P1 `ModelClass` + strategy dispatch, domain only; P2 large-tier re-summarisation — the rolling summary rebuilt from the source messages instead of from its own previous output, gated on `ModelClass::Large` and wired into `run_compaction_pass`, so P1 now has its first consumer; P3 age-weighted retention — `compaction_verbatim_days` plus a tool-result rung between "leave it alone" and "drop the whole turn", fed by `Message::created` on the live Goose trim path and firing only when a conversation is already over budget; P4 compact-on-resume gate, wired to the session reopen; P6 `should_compact` now moves the server between turns, rate-limited by `claim_compaction`, and `reset_session` has its first production caller); **P5 code landed 2026-08-06, awaiting the warm-versus-cold TTFT measurement that decides it** (`prefix_cache.rs`: `PrefixCacheState` + the six `InvalidationReason`s recorded in `goose_agent.rs`, `Agent::prefix_cache_state` defaulting to `None`, and the trimmer's age rung now firing on a cold prefix as well as over budget — the warm half was already P3's); **P7a (the API half) LANDED 2026-08-06** (`POST /sessions/{id}/compact` — the manual axis, running the same `run_compaction_pass` behind the same `claim_compaction` rate limiter as P6, and reporting a `status`/`reason` pair with the session's real utilisation when it refuses); **P7b (the desktop control) PARTIALLY LANDED 2026-08-06** — the bullet said "a control on the existing `ContextCard`", but `ContextCard.tsx` is the MCP-UI tool-result renderer and the shipped app had no context-pressure surface at all, so P7b is a new surface across the chat views, not a button. `context_warning` is now in the `ChatEventType` union, `PondApiClient.compactSession()` posts to the P7a endpoint, and a shared `ContextPressureNote` renders the pressure line and a "Compact now" control in `hub/views/ChatHub.tsx`. No Rust changed — the frame already reached the client, so "zero consumers" was a *rendering* fact, not a transport one. `sections/Canvas.tsx` remains a deliberate deferral. **P7b-fix LANDED 2026-08-07 (`1a4dda59`), closing both of the open defects this row used to carry.** The first was that the "Compact now" control could never reach `status: "compacted"` on any default install: P6's pressure axis took the shared `claim_compaction` quota one statement after emitting the frame and strictly before the button was rendered, so six pressured turns gave six `cooling_down` refusals and never one pass — unreachable, not merely uncommon. `claim_manual_compaction` differs in exactly one respect, skipping the turn cooldown; it still requires the session, still recomputes `should_compact` under the same lock, and still STAMPS `turns_at_last_compaction`, so a press rations the automatic axis exactly as an automatic pass would and the two cannot double-spend the summariser. What bounds the manual axis is `SessionSummaryService::refresh` answering `NothingToDo` from the through-pointer before it reaches `provider.complete` — not the cooldown. `compaction_in_flight` is untouched and still read before the claim, which is what protects the serial on-device engine. The second was that the phase's one load-bearing guard was a two-substring grep of `ChatHub.tsx` catching only a textual revert, with nothing in the suite ever rendering `ChatHubView`: `sections/Chat.tsx` now has the branch and the render it was blocked from, with a real render test, and the grep covers both surfaces and says in the file that it is a tripwire, not coverage |
| [05](./pai/05-reasoning-and-thinking.md) | Reasoning and thinking | Ability to think | 03, 04 | **P1, P2, P3, P4, P6 LANDED** (P1, P2 2026-08-06; P4, P6 2026-08-07; P3 predates the programme) (P1 the reasoning channel — `GooseAdapter` lifts `MessageContent::Thinking` out of every `AgentEvent::Message` and yields `AgentStreamEvent::Thinking`, gated once at the PRODUCER on `show_thinking && !voice` so all three consumers inherit it, including the CLI voice printer that a seam-side gate would have missed; the producer always existed upstream in `goose-local-inference` and was being discarded at `as_concat_text()`, so the doc's own diagnosis pointed at the quarantined crate and was wrong. P2 producer + store — `reasoning_tokens` on `UsageStats`/`TurnStats`/`SessionMessage` plus migration 0039, nullable with NO `DEFAULT` because "nobody counted" and "counted zero" are different facts; counted through PAI-3's `TokenCounter` port, ungated by `show_thinking`, and reported ALONGSIDE `completion_tokens`, never deducted). **P1's granularity defect is FIXED 2026-08-07** (`76653347`): `ReasoningCoalescer` emits one frame per passage rather than one per token, which on local/gguf had been rendering hundreds of one-fragment paragraphs. **P4 LANDED 2026-08-07** (`8689ba47`): `reasoning_effort` tri-state rendered into the prompt as a word budget derived from the window — deliberately NOT a `CompactionProfile` field, because every production site goes through `from_context_window`, which would have minted fifteen profiles carrying an effort nobody chose. **P6 LANDED 2026-08-07** (`bc3aa9df`): persist + rehydrate thinking, migration 0040 as a SIDE TABLE so that no prompt-building `SELECT` can hand a model its own discarded scratch work, gated once inside `record_thinking` rather than at the two call sites (which is how P1 leaked `is_voice`), opt-in and off by default, and guarded against replay by `crates/pond-core/tests/thinking_is_never_replayed.rs`. **Outstanding: P5** (`output_reserve_tokens` derived from measured reasoning behaviour — needs the Orin) **and P7** (unify the two stream handlers). P2's `turn_stats` SSE frame and `GET /usage/summary` are still NOT done |
| [06](./pai/06-multi-agent-orchestration.md) | Multi-agent orchestration | Multi-agent orchestration | 01, 02, 03, 04 | **P1, P2, P3 LANDED** (P1 `e7c9fd9a`, P2 `3306943a` 2026-08-07; P3 `9c50d867` 2026-08-09). **P1** the orchestration domain and the `Orchestrator` port, `pond-core` only — no adapter, no migration, no call site. Its whole intent was to make *a subagent's scope is a subset of its parent's* structural rather than checked: `TaskRequest` has no scope, tool, depth or session field and is `deny_unknown_fields`; `RolePersonalData` has no widening variant so the profile axis needs no runtime test; `DelegationDepth` cannot be constructed from a number, deserialized or defaulted, and its only increment is private, so a child is HANDED an authority and can never forge one. The tool axis stays a runtime intersection, which is the honest limit of what the type system could carry — `grants_tool` is deliberately stricter than `filter_tools_by_groups`, denying an empty set and an unknown prefix, because Goose reads an empty `available_tools` as ALL TOOLS. **P2** `GooseOrchestrator` over a child loop GIAP owns. `run_subagent_task` is `pub` inside a `pub(crate) mod` and therefore unreachable, but the reason not to patch its visibility is better than the reason not to reach it: it calls `build_subagent_prompt`, which renders Goose's own `subagent_system.md` unconditionally onto an `Agent` it builds internally and never returns, so the patched version would ship a child introducing itself as "a specialized subagent within the goose AI framework" — and `GiapProviderShim` would not correct it, because `GOOSE_DEFAULT_MARKER` is not in that template. The owned loop is ~120 lines and the fork patch set stays at **five**. Invariant 2 also went from three hardcoded copies of the ten stripped builtins to one, with a canary. **P3** scope inheritance: `DelegationAuthority::for_turn` built at the edge from the turn's real post-selection, post-guest-subtraction allow-set — the same binding published to `ShimControls`, because passing the catalog makes every downstream intersection a no-op and that is exactly how PAI-1 P5 shipped inert — plus `narrow_child_groups`, which subtracts what the CHILD's scope denies. That subtraction is the phase: P1 derived scope and tools as two independent statements, so a `Household` parent running a `personal_data: deny` role produced a child whose scope said `Guest` while it still held `giap-memory`, whose MCP tools carry no session and read the household's memory regardless of any scope. The draft gate is answered by WITHHOLDING (`groups_denied_to_subagents`), not by checking: `engine_session_map.session_id` is the PRIMARY KEY so a child cannot be mapped to its parent without losing the parent's pairing, and a mapping that worked would hand a deny-role child its parent's scope straight back. P3 also fixed two silent P2 defects — the shim was rebuilding a child's delegation envelope away on every provider call, and a stream of children could evict a live parent's allow-set. **Outstanding: P4-P8**, and nothing calls `spawn` yet — P5's `delegate` tool is the first caller, and it is what turns the turn-authority registry from a published fact into an answered question |
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
4. **Egress is tracked at the adapter, and now gated there too.** Any crate that reaches the
   network with its own `reqwest::Client` goes through `egress::begin` / `EgressCall::finish`
   (PAI-2 P5), which checks `network_mode` before the socket opens and records the call either way.
   Verified 2026-08-05: `crates/pond-core/tests/egress_guard.rs` fails the build for an HTTP-sending
   source file that is in none of its three classification lists. The old wording — "copy
   `traced_send` from `pond-adapters-weather/src/lib.rs:15-30`" — was wrong twice over: the line
   range had rotted by nine lines, and copying a helper is not a guarantee.
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

All of these are now **done**. Kept as a record of what was corrected and why.

| Document | Claim | Reality | Status |
|---|---|---|---|
| `architecture/token_tracking.md` ("Estimation") | "Real token counts are not available from Goose's `AgentEvent` stream" | Migration `0029_message_token_counts` and `TurnStats` landed; they are | FIXED; rewritten again by PAI-3 P6, whose rewrite moves the old line 28 |
| `architecture/scheduling.md:38` | `TaskKind` has two variants; seven MCP tools | Three variants (`SensorTrigger`); twelve tools | FIXED |
| `api.md:219` | "Token validation is currently a stub" | `SqliteHandshakeAdapter::validate_token` is real | FIXED |
| `architecture/model_capabilities.md` (`ModelCapabilities` struct) | `ModelCapabilities` has five fields | Six — `tool_calling` was missing from the doc | FIXED; the detection table below it still omitted the column until PAI-3 P6 |
| `security/ports/policy.rs:6-11` | Describes the unconditional loopback bypass "at lines 206-214" | Removed in #94; now gated behind `POND_DEV_ALLOW_LOOPBACK` | FIXED |
| `CLAUDE.md` | "14 `giap-*` extensions", "57 tools" | **15** and **61**. I twice got this wrong before counting properly: `giap-toolkit` registers via the `TOOLKIT_EXTENSION` const, so a grep for `"giap-*"` string literals misses it. Count `register_builtin_extension(` call sites instead | FIXED 2026-08-04 |

---

## 5. What this programme deliberately does not do

- **It does not replace Goose.** `pond-agent` + `pond-inference` remain the quarantined
  independent path. Every design here works through the live `GooseAdapter` and keeps its domain
  logic in `pond-core` so a future swap stays possible.
- **It does not add cloud inference.** `cloud_fallback_enabled` stays off by default and no
  workstream here depends on it. PAI-8 ingests from cloud accounts; it never sends prompts to them.
- **It does not promise full-database encryption.** PAI-2 encrypts the high-sensitivity stores and
  writes down the SQLCipher migration path with its cost, rather than claiming a property the
  build system cannot currently deliver on a Jetson cross-build. As of P4 (2026-08-05) that means
  `secrets.json` only: both SQLite databases are still plaintext, and by default the key sits in
  the same directory as the ciphertext, so this protects a copied file rather than a stolen board.
- **It does not promise first-party WhatsApp.** PAI-8 is explicit about which messaging sources
  have a sane official read API and which require a bridge the user runs themselves.
