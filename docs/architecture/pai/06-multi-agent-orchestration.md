# PAI-6 — Multi-agent orchestration

Requirement: *multi-agent orchestration.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-1](./01-identity-and-profile-boundaries.md) (whose data a subagent may touch),
[PAI-2](./02-privacy-and-security-guardrails.md) (what it may do),
[PAI-3](./03-context-governor.md) and [PAI-4](./04-smart-compaction.md) (what it may spend).

Verified against code **2026-08-07**, in a checkout with the submodule initialised at
`29ef609c` (`heads/main`). Every Goose-side claim below was read out of `goose/crates/goose/src/`
in this tree; the submodule is uninitialized in a fresh clone, so run
`git submodule update --init --recursive` before re-checking, and note that CI clones the fork
branch tip directly rather than this SHA.

> **Read this before implementing any phase.** The 2026-08-07 re-verification found **six**
> current-state claims in this document that were false, three of which would have made an
> implementer write code that does not compile or does nothing. They are corrected in place and
> called out where they were. The largest is in section 1.2: **`run_subagent_task` is not
> callable from `pond-adapters-goose`.** P2 is a fork-patch decision before it is a coding task.

---

## 1. What is true today

### 1.1 GIAP has the vocabulary and none of the machinery

- **`DelegatingAgent`** — `pond-core/src/shared/services/delegation.rs`, **295** lines (this said
  293). An `Agent` that holds `Vec<(MatchFn, Arc<dyn Agent>)>` plus a fallback and routes by first
  match on message text, preserving `session_id`. Its own module doc (the paragraph beginning
  "NOTE: this is a neutral routing primitive") warns against wiring it with hardcoded keyword
  matchers. It is a **switch, not a spawner** — no parallelism, no aggregation, no task lifecycle.
  Deliberately unwired: `grep -rn DelegatingAgent crates/` finds it only inside its own file.

  **It is not a base for `Orchestrator`, and P1 did not treat it as one.** Its unit of dispatch is
  a pre-constructed `Arc<dyn Agent>`, so it can select between children but never *derive* one from
  a parent, which is the only thing orchestration needs. It forwards the `AgentRequest` unchanged,
  which hands the child the parent's `profile_scope` and `session_id` verbatim — invariant 1 and
  invariant 4 both inverted. And its selection input is message text, which is the keyword
  classification the working agreement forbids. Leave it alone; do not refactor it.
- **`prompts/subagent_system.md`** — GONE (2026-08-06). It was a complete, GIAP-flavoured subagent
  system prompt registered as a Goose override by `giap_prompts.rs`, and GIAP never rendered or
  spawned it: the live prompt path is pond-core's `build_prompt_partition`. The whole of
  `giap_prompts.rs` and `src/prompts/` (12 templates) is deleted, so there is no longer a
  GIAP-authored subagent prompt to point at if orchestration is ever built.
- **`AgentRecipe`** — `user_data/domain/recipe.rs`, **six** fields:
  `{ id, name, description, yaml, active, created_at }` (this said five; same class of miscount as
  the "Profile has six fields not five" correction in the checklist's 2026-08-04 audit).
  `POST /api/v1/recipes/{name}/run` parses the YAML via `routes.rs :: RecipePrompt` — cite it by
  symbol, the old `routes.rs:9614` citation is dead — a four-field serde struct that reads two;
  the `goose::recipe::Recipe` call was the only `goose::` reference in `pond-api` and was removed
  to keep that crate framework-free. It extracts `prompt` or `instructions` and runs it through the
  **same chat-stream pipeline as an ordinary user message**. A recipe today is a stored prompt.

  **The table is `agent_recipes`, not `recipes`** (migration `0012_recipes.sql` names the file
  after the concept and the table after the type). Anyone implementing "roles live in the recipes
  table" from the old wording writes `SELECT ... FROM recipes` and gets `no such table` — or, worse,
  adds a migration creating one, which is exactly the persistence P1 forbids.
  `SqliteRecipeRepository::upsert` is `INSERT OR REPLACE … datetime('now')`, so editing a role
  **resets its `created_at`**. Do not build anything that trusts that ordering.
- **`ModelRouter`** — GONE. `model_router.rs` was never declared by a `mod` statement, so it was
  never compiled and its six tests never ran; three of them asserted routing behaviour its
  `complete` contradicted. Per-role dispatch, if wanted, starts from the live provider hot-swap
  seam in `pond-api`'s `AppState`, not from this file.
- **`UserSkill`** — named Markdown injected every turn, deliberately placed outside the KV-cache
  prefix hash. It no longer travels through `Agent::extend_system_prompt`: Goose rebuilt that into
  an appendix the provider shim discarded, and the entry was never removed when a skill was
  deactivated. The shim's per-session `turn_appendix` is now the only delivery path.

No `spawn_subagent`, no task queue, no worker pool, no parallel fan-out, no inter-agent messaging,
no result aggregation exists anywhere in `crates/`.

### 1.2 Goose has real machinery, and GIAP strips it

The `strip_list` in `goose_agent.rs` (grep the symbol; the old `:2466-2478` citation now lands in
`resolve_session_tool_groups`, which is unrelated) removes ten builtins from every session unless
the user explicitly added them: `developer`, `computercontroller`, `extensionmanager`, `todo`,
`apps`, `analyze`, **`summon`**, `summarize`, **`orchestrator`**, `tom`.

**There are two hardcoded copies of those names in that file** — `strip_list`, which is the guard,
and the `is_builtin` closure further down, which carries the same ten plus `default` and
`suggestions`. Only the first enforces anything. A canary test that pins one proves nothing about
the other; pin both.

What is being stripped:

| Goose surface | What it provides |
|---|---|
| `goose/crates/goose/src/agents/subagent_handler.rs` | `run_subagent_task(SubagentRunParams { config, recipe, task_config, return_last_only, session_id, cancellation_token, on_message, notification_tx })` — a complete child agent loop with cancellation and a notification channel. **`pub(crate)`. See below.** |
| `.../subagent_task_config.rs` | `TaskConfig { provider, model_config, parent_session_id, parent_working_dir, extensions, max_turns }` — the module is `pub(crate)` but the type is re-exported, so this one IS reachable |
| `.../platform_extensions/summon.rs` | `load` (list/load subrecipes, recipes, agents) and `delegate` (ad-hoc or source-based subagent, `async: true` for background, then `load(taskId)` to collect) |
| `.../platform_extensions/orchestrator.rs` | `list_sessions`, `view_session`, `start_agent`, `send_message`, `interrupt_agent` over Goose's session manager |
| `.../subagent_execution_tool/` | **serde types only.** `mod.rs` is three lines and `notification_events.rs` holds `TaskStatus`, `TaskExecutionNotificationEvent`, `TaskExecutionStats`, `TaskInfo`, `FailedTaskInfo`. This row used to say "task execution plumbing"; there is no executor, no queue and no scheduler here. P8 has nothing to reuse |
| `.../recipe/` | full Recipe model, validation, templating, subrecipes |

#### CORRECTION (2026-08-07): `run_subagent_task` is NOT callable, and this changes P2

`goose/crates/goose/src/agents/mod.rs` declares:

```rust
pub(crate) mod subagent_handler;
pub(crate) mod subagent_task_config;
```

and its re-export block exports exactly `subagent_handler::SUBAGENT_TOOL_REQUEST_TYPE` and
`subagent_task_config::TaskConfig`. So `run_subagent_task`, `SubagentRunParams` and
`OnMessageCallback` are **crate-private to `goose`** and unreachable from `pond-adapters-goose`.
The only caller in the whole Goose workspace is `platform_extensions/summon.rs` — the extension
GIAP strips.

This document previously closed section 3.1 with "the part worth having is `run_subagent_task`,
which is callable directly." It is not. P2 must choose, **before writing code**, between:

1. **A sixth GIAP fork patch** — two `pub(crate)` → `pub` plus a re-export — staged on
   `jarida-io/Goose` per `docs/goose-patch-management.md`, with the CI fork-branch-tip bump. Note
   that `goose-patch-management.md` already carries **five** rows while `CLAUDE.md` still describes
   the patch set as two; that staleness is P2's to clear if it takes this route.
2. **Reimplementing the child loop** over the public `Agent` API (`Agent::with_config`,
   `update_provider`, `add_extension`, `apply_recipe_components`, `override_system_prompt`,
   `reply`), which is entirely possible from re-exported symbols and costs ~120 lines in
   `pond-adapters-goose` that duplicate `get_agent_messages` — and which is the only route that
   lets GIAP set the child's system prompt (see section 3.2).

So the expensive, fiddly part — spawning a child agent with its own conversation, capping its
turns, cancelling it — exists, is switched off, **and is behind a visibility wall**. What is
genuinely free is the structure: `Agent::with_config` builds an `ExtensionManager` with **no**
extensions and nothing auto-loads defaults into it, so a child starts with an empty tool surface
and gets exactly what the adapter puts in `TaskConfig.extensions`. Invariants 1, 2 and 6 are
achievable by construction there — provided the extension list is built from GIAP's own
`registered_extensions()` and never from `EnabledExtensionsState::extensions_or_default`, whose
fallback re-arms every `default_enabled` platform extension including `summon`.

#### Four more Goose-side facts an implementer needs, and will otherwise find the hard way

- **`available_tools: vec![]` means ALL TOOLS.** `ExtensionConfig::Builtin.available_tools` is a
  real per-extension allowlist enforced twice (listing, and inside `dispatch_tool_call`), but its
  predicate is `available_tools.is_empty() || available_tools.contains(&tool_name)`. GIAP's own
  `builtin_extension_config` helper passes `vec![]`. Reusing it for a child is a one-line
  scope-widening default.
- **The provider shim does not veto a subagent's tools.** `GiapProviderShim` resolves its per-turn
  allow-set from `session_context::current_session_id()` via `ShimControls::existing_session`,
  which does not create entries. A child runs under its own Goose session id, for which GIAP has
  published nothing, so `enforce_tools(tools, &None)` is a no-op — and because the turn appendix is
  also `None`, the `system_appendix_dropped` warning does not fire either. It fails **silently**.
- **`TaskConfig { max_turns: None, .. }` panics.** `build_subagent_prompt` does
  `.expect("TaskConfig always sets max_turns")`. GIAP will construct `TaskConfig` by struct literal
  (because `TaskConfig::new` reads Goose's global `Config`), so it must always pass `Some(n)`.
- **A cancelled child returns `Ok`, and so does one that ran out of turns.** The reply loop breaks
  and returns `Ok(partial_text)`; an exhausted `max_turns` yields the literal `MAX_TURNS_MESSAGE`
  string as the "result". A test asserting "cancel works" by checking for an `Err` is vacuous.
  PAI-6 P1's `TaskStatus` has `Cancelled` and `TurnBudgetExhausted` as distinct variants, and
  `TaskRun::result_for_parent` returns `None` for both, precisely so the adapter has somewhere
  correct to put the distinction.

---

## 2. The gap

GIAP has one agent that does everything in one context window, with one tool surface, at one
priority. Anything that needs a different persona, a different tool scope, a different model, or an
isolated context has nowhere to run.

---

## 3. Recommendation: reuse the executor, own the policy

The question is whether to build orchestration in `pond-core` or re-enable Goose's. **Neither
alone. The split is: `pond-core` owns *what may run and under what limits*; the Goose adapter owns
*how a child agent is executed*.**

### 3.1 Why not simply re-enable `summon` and `orchestrator`

1. **They are coding-agent shaped.** `TaskConfig` carries a `parent_working_dir`; `summon`
   discovers "sources" by scanning the filesystem; the `delegate` tool description advises
   "partition files strictly — no two delegates touch the same file". None of that describes a home
   assistant, and it is the tool text a 2-4B on-device model will read and try to act on.
2. **They bypass every boundary this programme is building.** They do not know about GIAP's
   tool-group narrowing (`mcp/domain/tool_group.rs`), the always-on draft gate, or profile scope.
   `orchestrator.start_agent` would hand a fresh agent the full 61-tool surface that Phase D spent
   an entire phase narrowing to 17.
3. **They are surface, and surface is the part that must survive a fork swap.** The programme's
   invariant 5 exists for this: anything that would need rewriting if Goose were replaced belongs in
   `pond-core`.

Keeping them stripped costs nothing. What is worth having is the child agent loop — and reaching it
is a decision, not a call; see the correction in section 1.2.

### 3.2 The domain — AS LANDED (P1, 2026-08-07)

`pond-core/src/shared/domain/orchestration.rs` + `pond-core/src/shared/ports/orchestrator.rs`.

**Not `pond-core/src/agents/`**, which this document used to propose. `lib.rs` declares exactly
six top-level modules and each is a bounded context; orchestration spans models (provider, window),
mcp (tool groups) and user_data (recipes, profiles), which is the definition of `shared/` in this
tree — where `chat`, `delegation`, `session_summary` and `egress` already live.

```rust
pub struct AgentRole { /* private */ }        // name, instructions, tool_groups,
                                              // personal_data, max_turns, context_fraction
pub enum RolePersonalData { Inherit, Deny }   // NO variant widens
pub struct DelegationDepth(u8);               // no Deserialize, no Default, private increment
pub struct DelegationAuthority { /* private */ }   // session_id, scope, tool_groups, depth
pub struct TaskRequest { pub role, pub instructions, pub inputs }  // deny_unknown_fields
pub struct TaskSpec { /* private, derived */ }
pub enum TaskStatus { Queued, Running, Completed, Cancelled, TurnBudgetExhausted, Failed }
pub struct TaskRun { id, role, parent_session_id, status, result, error, started_at, finished_at }

impl DelegationAuthority {
    pub fn root(session_id, scope, tool_groups) -> Self;
    pub fn delegate(&self, role: &AgentRole, request: TaskRequest)
        -> Result<TaskSpec, DelegationRefused>;
}
impl TaskSpec {
    pub fn grants_tool(&self, tool_name: &str) -> bool;
    pub fn child_authority(&self, child_session_id) -> DelegationAuthority;
}

#[async_trait]
pub trait Orchestrator: Send + Sync {
    async fn spawn(&self, spec: TaskSpec) -> Result<TaskRun>;
    async fn poll(&self, task_id: &str) -> Result<Option<TaskRun>>;
    async fn cancel(&self, task_id: &str) -> Result<()>;
    async fn list(&self, parent_session_id: &str) -> Result<Vec<TaskRun>>;
    async fn cancel_children_of(&self, parent_session_id: &str) -> Result<usize>;
}
```

Four differences from the sketch above it, each for a reason:

- **`AgentRole` has no `profile_scope` and no `model_role`.** An absolute `ProfileScope` on a
  stored role is a widening waiting to happen — a role saying `household` would widen a `Guest`
  parent — and which member is speaking is a per-turn fact that a stored config cannot know.
  `RolePersonalData` replaces it: `Inherit` gives the parent's scope exactly, `Deny` drops to
  `Guest`. **There is no value of that type that widens**, so invariant 1's profile axis needs no
  runtime check. `model_role` is deferred to P7, which has to define the type first (see below).
- **`TaskSpec` is derived, never constructed.** Its fields are private, it has no `Deserialize`,
  and the only producer is `DelegationAuthority::delegate`. Everything the child may do is computed
  from the parent and the role.
- **`TaskRequest` — the part the model supplies — carries no authority at all**, and
  `deny_unknown_fields` makes that enforced rather than ignored: a `delegate` call carrying
  `profile_scope`, `tool_groups`, `depth` or `session_id` is a parse error the model can see, not a
  field silently dropped.
- **`spawn` takes no `parent: &SessionRef`.** The spec already carries the parent's session id.
  Passing it twice lets the two disagree, and the one nearer the engine wins — which is how a scope
  gets widened by accident.

`pond-adapters-goose` will implement `Orchestrator` (P2), translating an `AgentRole` into a
`Recipe` plus a `TaskConfig`. **It cannot render a GIAP subagent system prompt.** GIAP's
`prompts/subagent_system.md` was deleted on 2026-08-06 with the whole of `giap_prompts.rs`, and
Goose's own copy — `goose/crates/goose/src/prompts/subagent_system.md`, opening "You are a
specialized subagent within the goose AI framework, created by AAIF" — is rendered
**unconditionally** by `build_subagent_prompt`, which then calls `override_system_prompt` on a
child `Agent` it constructs internally and never returns. There is no seam. The only
caller-controlled input is `Recipe.instructions`, which lands inside that template as
`{{task_instructions}}`. So P2's real options are: accept a hybrid, goose-branded child prompt
(the shim will **not** correct it — its `GOOSE_DEFAULT_MARKER` does not appear in that template);
patch the submodule; or take route 2 from section 1.2 and own the loop. Feed pond-core's
`build_prompt_partition` output into `Recipe.instructions` either way.

### 3.3 What is reused rather than rebuilt

| Need | Existing code |
|---|---|
| Role definitions | `AgentRecipe` rows in the **`agent_recipes`** table — role fields ride one GIAP-owned YAML key, `giap_role`, which `RecipePrompt` and Goose's `Recipe` both ignore |
| Reusable instruction blocks | `UserSkill` |
| Per-role tool narrowing | `mcp/domain/tool_group.rs` + `services/tool_selection.rs` — but use `filter_tools_by_groups` (a pure intersection), **never** `select_groups`, whose every failure path widens by design |
| Per-role model | **Nothing.** `ModelRouter` does not exist and never compiled; there is no `ModelRole` type. See P7 |
| Cheap in-process routing | Nothing. `DelegatingAgent` is a dead end — see section 1.1 |
| Child agent execution | Goose's child loop, subject to the visibility wall in section 1.2 |
| Subagent system prompt | **Nothing.** GIAP's is deleted; Goose's is not substitutable. See section 3.2 |
| Concurrency limiting | The `Semaphore` pattern from `schedule_executors.rs` |

The genuinely new code is the domain types, the port, the adapter translation layer, and one MCP
extension.

### 3.4 On-device reality: concurrency 1, and that is fine

One GPU, one resident model. Two subagents on a Jetson do not run in parallel; they interleave
badly, thrash the KV cache, and finish later than if they had run one after another.

So: **default concurrency 1 for anything that runs on this device; parallelism only for a provider
that runs somewhere else.**

**The predicate is `model_class::runs_on_this_device`, not `context_governor::is_local_provider`.**
The first covers `local`, `gguf`, `ollama` and `llamafile`; the second covers only `local` and
`gguf`. Ollama and llamafile speak HTTP, but on a GIAP pond they speak it to `127.0.0.1` — the same
GPU the parent's next turn needs. PAI-4 P2 already had to fix exactly this mistake once. A test
pinning concurrency 1 for `"local"` alone is a vacuity control pinning the wrong number and would
stay green straight through the regression. `max_concurrent_subagents` in the P1 domain is written
against the wider predicate and its test iterates `ON_DEVICE_PROVIDERS` so a provider added later
is covered without anyone remembering the file.

**Concurrency 1 is necessary and not sufficient.** `goose-local-inference`'s `LoadedModel` holds
`session: Option<SessionKv>` — exactly ONE retained KV prefix per loaded model slot, process-wide,
behind one async mutex. An interleaved subagent turn does not merely queue behind the parent: it
**overwrites the parent's prefix**, so the parent pays a full re-prefill (the measured 3.7 s on the
Orin) on its next turn. The semaphore therefore has to serialise subagent turns against **parent**
turns, not just against each other. Note also that a synchronous subagent runs inside a held
`sse_semaphore` permit, and that semaphore is `Semaphore::new(4)`: four concurrent delegating turns
exhaust the entire interactive chat pool.

The value of delegation on-device is therefore **not** wall-clock speed. It is:

- **Context isolation.** A subagent burns its own window on a messy sub-problem — twelve tool calls,
  a long document — and returns three sentences. The parent's context grows by three sentences
  instead of twelve tool results. On a 4K window that is the difference between finishing the task
  and being compacted mid-conversation.
- **Tool isolation.** A role gets four tools instead of fifty-nine, which is measurably better tool
  selection on small models and a smaller prompt (Phase D measured 6,539 → 2,386 prompt tokens for
  59 → 17 tools).
- **Failure isolation.** A subagent that loops burns its own turn budget and returns an error.

Saying this plainly matters because "multi-agent" invites an assumption of parallelism, and shipping
serialised parallelism would be complexity with no payoff.

### 3.5 Budget: a subagent is a second claim on one window

Each spawn takes `context_fraction` of the parent's budget via the governor. The parent's own
history budget shrinks while a child is live. Without this, two agents each believe they own the
whole 4K window and the second one's first turn overruns.

**Where that reservation goes, verified 2026-08-07.** The seam is not `ContextGovernor`, which is
stateless and only answers "how big is this model's window". It is
`CompactionProfile::for_windows`, whose single live producer is `GooseAdapter::profile_for`, and
the only consumer that trims is `turn_trimmer::trim_history` — one call site outside its own file.
The reservation must shrink **`history_token_budget` only**. Scaling the resolved window before
`ContextGovernor::prompt_window` would shrink the preamble clamp too and move the KV prefix, which
costs the 3.7 s re-prefill. Good news found while checking: `memory_token_budget` feeds
`memory_block_for_user_msg`, which rides the **user** message, so shrinking it does not move the
prefix either. `turn_profile()` is stateless and per-turn, so P4 needs new adapter-owned state that
it reads; keep the live-child ledger in the orchestrator and pass a reservation in, rather than
putting a mutable budget on `CompactionProfile`.

Nested delegation is capped at depth 1 in v1. A subagent may not spawn. P1 makes that structural
rather than a counter: `DelegationDepth` has no public constructor from a number, no `Default` and
no `Deserialize`, and the only public path to a deeper authority is
`DelegationAuthority::delegate` → `TaskSpec::child_authority`, which refuses at
`MAX_DELEGATION_DEPTH`. A child is handed its authority; it never builds one, so there is no value
for it to get wrong. The strongest enforcement remains structural in the adapter too: if
`giap-orchestrator` is simply not in a child's `TaskConfig.extensions`, the child physically has no
`delegate` tool.

### 3.6 The tool surface

One new extension, `giap-orchestrator`, behind `ext_orchestrator_enabled` (making 16 registered
extensions — there are **15** today, not 14; `giap-toolkit` registers via the `TOOLKIT_EXTENSION`
const, so grepping for `"giap-*"` string literals undercounts it):

- `list_roles` — what personas exist, and what each is for.
- `delegate` — run a role against instructions. Synchronous by default; `background: true` only when
  the provider supports concurrency.
- `check_task` — poll a background task.

Three tools, not five, and the descriptions are written for a home assistant. The extension joins
the core group set only when enabled, so it costs nothing in the common case.

Two things this extension must NOT do. It must not appear in `dispatcher.rs` or in
`DIRECT_DISPATCH_ALLOWLIST`: that path runs a tool with no chat turn and therefore no caller, and
delegation decides and executes. And if a subagent can ever run under a `Guest` parent,
`giap-orchestrator` belongs in `groups_denied_to_guests()` — delegation is a way to reach a tool
you were denied.

### 3.7 Streaming

`AgentStreamEvent::SubagentProgress { task_id, role, status, detail }`, so the desktop can render
the tree instead of a spinner. Subagent output is **not** persisted into the parent's
`session_messages` — only its final result, as a tool result, which is what keeps the context
isolation real.

**`notification_tx` cannot feed this on its own**, and the earlier wording that said it could would
have produced a progress stream that fires only when a child happens to call a tool and shows
nothing at all for a text-only subagent. The channel receives exactly one shape,
`{"type":"subagent_tool_request","subagent_id":…,"tool_call":{"name","arguments"}}`, emitted only
for `MessageContent::ToolRequest`. There is no status, no turn counter, no tool result and no
completion event on it. P6 needs both channels: `on_message` (a synchronous `Fn(&Message)` called
inline in the drain loop) for lifecycle and turn counting, `notification_tx` for the tool name.
Two consequences: the notification carries `tool_call.arguments` **verbatim**, which on this pond
can include household memory queries and device state, so P6 forwards the tool NAME and drops the
arguments (PAI-2's minimisation rule); and `on_message` sees `MessageContent::Thinking`, so PAI-5's
reasoning gate — which lives at the `GooseAdapter` producer, a path this does not go through — has
to be re-applied here or a subagent's reasoning reaches the consumer.

Also compiler-forced: a new `AgentStreamEvent` variant breaks four exhaustive matches
(`routes.rs :: chat_stream`, `routes.rs :: agent_chat_stream`, `chat.rs`'s voice/workflow consumer,
and `main.rs`'s CLI renderer) but **not** `pond-agent/src/agent.rs` or `delegation.rs`, both of
which use `_ => {}` — and `pond-agent` is `cargo check`-only in CI. The two `routes.rs` blocks are
not identical (PAI-5 P7's unification is still open), so P6 writes its arm twice and a test that
only drives `/chat/stream` proves nothing about `/agent/chat/stream`.

---

## 4. Phases

- **P1 — LANDED 2026-08-07.** Domain: `AgentRole`, `TaskSpec`, `TaskRun`, `Orchestrator` port.
  Roles stored in the existing **`agent_recipes`** table; no new persistence, no migration.

  I built this to make the two invariants that matter structural rather than checked. `TaskRequest`
  — the part a model supplies — has no scope, no tool and no depth field, and it is
  `deny_unknown_fields`, so a `delegate` call that tries to carry one is a parse error rather than
  a silently-dropped field. The child's scope comes from `RolePersonalData`, an enum whose codomain
  is "the parent's scope" and "Guest" and **contains no widening value at all**; `DelegationDepth`
  has no `Deserialize`, no `Default` and no public constructor from a number, so a child cannot
  forge one. `TaskSpec` has private fields and exactly one producer.

  **Respecified against the phase text, and why.** The design's `AgentRole` carried
  `profile_scope: ProfileScope` and `model_role: ModelRole`. I dropped both. An absolute scope on a
  stored role is the widening hazard the whole workstream is about — a role saying `household`
  would widen a `Guest` parent — and which member is speaking is a per-turn fact a stored config
  cannot know; the only role-level restriction that is meaningful *and* monotone is "drop to
  Guest", which is what `RolePersonalData::Deny` is. `ModelRole` does not exist and `ModelRouter`
  never compiled, so defining a type here would have been P7 done badly and early. I also dropped
  `spawn`'s second `parent: &SessionRef` argument: the spec already carries the parent session id
  and two sources of truth for an authorisation input is exactly how one gets widened.

  **What I rejected.** Refactoring `DelegatingAgent` into the port — it forwards the request
  unchanged, which is invariants 1 and 4 inverted, and it selects on message text, which is the
  keyword classification the working agreement forbids. A no-op adapter to make the port look
  wired — this programme already has three correct-but-unreachable mechanisms and each cost a
  later round more than the gap would have. Adding a `roles` table: the role fields ride one
  GIAP-owned YAML key, `giap_role`, inside an existing `agent_recipes` row, and both readers of
  that YAML (`RecipePrompt`, Goose's `Recipe`) ignore unknown keys, so the recipe still runs as an
  ordinary routine.

  **Still not done, and owed by later phases.** Nothing calls any of it. The
  `DelegationAuthority::root(...)` call at the API edge, which is what makes the parent's real
  post-selection tool set the ceiling, is P3's. A role stored this way is also visible in the Hub's
  Routines list, because `hubDataStore.ts` maps every recipe to a routine card — a deliberate
  consequence of "no new persistence", not an oversight, and P5 should decide whether to filter it.
  `max_concurrent_subagents` has no caller until P8.
- **P2** `GooseOrchestrator`. **Decide the visibility question in section 1.2 first** — a sixth fork
  patch or an owned child loop — because "call `run_subagent_task`" does not compile. Translate
  role → `Recipe` + `TaskConfig`, always with `Some(max_turns)`, always with `GooseMode::Auto` (any
  approval-requiring mode deadlocks on the child's `confirmation_rx`), always with
  `return_last_only: true`, and always with an extension list built from GIAP's own
  `registered_extensions()`. Build the child's instruction text through `build_prompt_partition`
  and pass it as `Recipe.instructions`; note that `recipe.extensions` is **never read** by the
  child loop, so tool narrowing expressed there applies to nothing. Concurrency 1 per section 3.4.
- **P3** Scope inheritance: `DelegationAuthority::root` constructed at the edge from the turn's
  resolved scope and its **post-selection, post-guest-subtraction** tool set; tool groups narrowed
  per role; draft gate enforced inside subagents. Two traps. First, publish the child's allow-set
  into `ShimControls` **and** set `ExtensionConfig::available_tools`; the shim alone is inert for a
  session GIAP never chatted in, and an empty `available_tools` means *all tools*. Second, the
  draft gate cannot resolve a subagent at all today: `actor_for_engine_session` walks
  `engine_session_map`, which only `resolve_goose_session` writes, so a child's actor is `None` →
  `REASON_UNRESOLVED_ACTOR`, which under the default `PolicyMode::Audit` **proceeds**. P3 is a
  mapping task before it is a wiring task. Test against the DEFAULT configuration
  (`tool_selection_mode = "all"`), or it repeats PAI-1 P5 exactly.
- **P4** Budget: `context_fraction` through `CompactionProfile`, shrinking `history_token_budget`
  only (section 3.5); parent budget shrinks while a child is live; depth capped at 1.
- **P5** `giap-orchestrator` MCP extension + `ext_orchestrator_enabled`. The toggle needs its **own**
  `default_*` fn returning `false` and a `false` in the `impl Default for Settings` body — reusing
  `Settings::default_ext_enabled()` (which returns `true`) ships orchestration on for every install.
  Add a `ToolGroup` catalog entry in the same change: nothing tests that registration and catalog
  agree, and an uncatalogued extension is treated as a user-added MCP server that selection never
  narrows, so the one extension that should be least present would become the one that can never be
  selected away. Do **not** add it to `dispatcher.rs` or `DIRECT_DISPATCH_ALLOWLIST`; do add
  `giap-orchestrator__delegate` to `MUST_NEVER_BE_DIRECTLY_DISPATCHABLE`. Update the extension
  count in `CLAUDE.md` 15 → 16, **and fix the counting recipe there while you are in the file**:
  `grep -c 'register_builtin_extension(' giap_registration.rs` returns 15 and the import line has
  no open paren, so CLAUDE.md's "minus the import" yields 14 — the exact wrong number this
  programme has recorded twice.
- **P6** `SubagentProgress` streaming + desktop rendering. See section 3.7 for what the notification
  channel actually carries.
- **P7** Per-role model assignment. **There is no `ModelRouter` and no `ModelRole` type** — start
  from `GooseAdapter::ensure_provider_current`, which caches `(Arc<dyn Provider>, ModelConfig)` on
  a single `provider:model` key, plus the `think`/`task` rows of `model_role_assignments` that the
  settings write path never writes. `AgentRequest.model_role` already exists as the carrier and is
  inert: it is read once and echoed back in `Done`, and the block headed "GooseMode from
  model_role" is a constant. Note the collision PAI-6 never mentioned: a per-role model swaps the
  resident GGUF, which is a full reload plus a re-prefill on the Orin, so per-role dispatch on an
  on-device provider may be actively negative and collides with PAI-4 P5's `PrefixCacheState`.
- **P8** Background tasks for providers that do **not** run on this device (section 3.4), with
  `check_task`. There is no machinery to reuse: `InferencePool` is a port with zero
  implementations, `AppState.inference_pool` is hardcoded `None`, and Goose's
  `subagent_execution_tool/` is serde types. The existing `notification_tx` broadcast plus
  `GET /notifications/stream` is the right transport and needs no new route — though nothing in
  `pond-desktop/src` subscribes to it yet.

---

## 5. Invariants

1. A subagent's scope is a subset of its parent's. Never wider — not tools, not profile, not
   network. *(P1: structural for the profile axis — `RolePersonalData` has no widening value and
   `TaskRequest` cannot name a scope. Runtime intersection for the tool axis, with `grants_tool`
   denying an empty set and an unknown prefix.)*
2. Goose's `summon`, `orchestrator` and `todo` stay stripped. **Building orchestration does not
   mean un-stripping them.** The hazard is not that a direct child-loop call bypasses the strip —
   a fresh `Agent` has no extensions to strip — it is
   `EnabledExtensionsState::extensions_or_default`, whose fallback re-arms every `default_enabled`
   platform extension, `summon` included. Build the child's list explicitly.
3. Concurrency is 1 for every provider that **runs on this device** until measurement says
   otherwise — `local`, `gguf`, `ollama`, `llamafile`. This used to say "`local`/`gguf`", which is
   the narrow reading PAI-4 P2 already had to fix once. See section 3.4; and note the semaphore has
   to cover the parent's turns too, not only other subagents.
4. Subagent conversations never enter the parent's `session_messages`; only results do. Satisfied
   by construction — `ChatService` is the sole writer of that table and the child loop never
   touches it — with one caveat worth stating: the child's turns *are* persisted, into Goose's own
   `sessions.db` under the child's id. "Not in the parent's history" is true; "not written down" is
   not. Deleting a parent session must release its children's engine sessions, or every delegation
   leaks a row.
5. Every spawn is cancellable, and cancelling a parent cancels its children. **This is entirely new
   work.** Goose's own background path mints an unrelated root token, `child_token()` is unused
   anywhere in Goose, and GIAP's parent-turn token is a stack local inside a stream closure with no
   registry and no accessor. Hence `Orchestrator::cancel_children_of`.
6. Depth is capped. A recursive delegation loop on a home server is a fire. *(P1: `DelegationDepth`
   cannot be constructed from a number, deserialized, or defaulted; the only public path to a
   deeper one refuses at `MAX_DELEGATION_DEPTH`.)*

---

## 6. Deliberate deferrals

- **Nested delegation beyond depth 1.** Revisit when a real task needs it.
- **Agent-to-agent messaging.** Every use case so far is fan-out plus aggregation, which needs no
  peer channel.
- **Re-enabling Goose's `todo`.** Plausibly useful for plan tracking; evaluate separately, on its
  own merits, with its prompt text reviewed for the on-device models.
- **Learned role selection.** Roles are chosen by the model from `list_roles` in v1. A classifier is
  premature before there is usage data.

---

## 7. Verification

- **Unit** — scope inheritance: a role requesting a tool group its parent lacks gets the
  **intersection**, not the parent's set (the old wording would be a widening whenever the role was
  narrower than the parent); a role cannot express a wider profile scope at all, so there is
  nothing to deny. *(Landed in P1: `tool_narrowing_tests`, `scope_inheritance_tests`,
  `depth_tests`, and `scope_lattice_tests` on `ProfileScope::is_within`. Every one was
  mutation-tested — widening the lattice, making `Deny` return `Household`, raising the depth cap,
  turning the intersection into a union, and defaulting `grants_tool` to true all fail a named
  assertion.)*
- **Budget** — with a child live, the parent's history budget shrinks by `context_fraction`; assert
  that **either** the sum fits inside `usable_prompt_tokens` **or** the effective budget is exactly
  `MIN_HISTORY_TOKENS`. The unconditional form of that assertion is wrong: `trim_history` floors
  the budget at `MIN_HISTORY_TOKENS = 64` on purpose, so at small windows or large fractions the
  sum legitimately exceeds the window. A test written from the old wording either fails against
  correct code or, if the author picks a fraction that never hits the floor, passes at every value
  tried and stays green through exactly the regression it was meant to catch. Make the vacuity
  control a case that DOES hit the floor.
- **Integration** — delegate a research task on the Orin, assert the parent's context grows by the
  result only, and compare parent context growth against running the same task inline. That
  comparison is the entire on-device justification for this workstream and should be recorded as a
  measurement in this document once taken.
- **Cancellation** — cancel a parent mid-delegation; assert the child terminates and no orphan task
  remains. **Do not assert this by checking for an `Err`**: the child loop returns
  `Ok(partial_text)` on cancellation and the literal `MAX_TURNS_MESSAGE` on budget exhaustion, so
  an `Err`-based assertion is vacuous. Assert on `TaskStatus`, which has distinct `Cancelled` and
  `TurnBudgetExhausted` variants for this reason, and on `TaskRun::result_for_parent()` being
  `None`.
- **Canary** — a test reading the fork's builtin list so that a Goose sync adding a new
  orchestration builtin fails loudly rather than silently re-enabling it, in the spirit of
  `goose_cap_message_is_still_verbatim`. Pin **both** copies of the ten names in `goose_agent.rs`
  (`strip_list` and the `is_builtin` closure); a test that reads one proves nothing about the
  other.
- **Registration/catalog cross-check** — nothing today asserts that
  `giap_registration.rs`'s registered extensions and `tool_group.rs`'s `TOOL_GROUPS` describe the
  same set. They happen to agree at 15. P5 owes that test, because the failure mode is silent and
  it widens.
