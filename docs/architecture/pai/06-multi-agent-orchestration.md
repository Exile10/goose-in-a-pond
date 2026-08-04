# PAI-6 — Multi-agent orchestration

Requirement: *multi-agent orchestration.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-1](./01-identity-and-profile-boundaries.md) (whose data a subagent may touch),
[PAI-2](./02-privacy-and-security-guardrails.md) (what it may do),
[PAI-3](./03-context-governor.md) and [PAI-4](./04-smart-compaction.md) (what it may spend).

Verified against code 2026-08-03. Goose-side claims verified against `jarida-io/Goose:main`; the
submodule is uninitialized in a fresh clone, so run `git submodule update --init --recursive` to
re-check locally.

---

## 1. What is true today

### 1.1 GIAP has the vocabulary and none of the machinery

- **`DelegatingAgent`** — `pond-core/src/shared/services/delegation.rs`, 293 lines. An `Agent` that
  holds `Vec<(MatchFn, Arc<dyn Agent>)>` plus a fallback and routes by first match on message text,
  preserving `session_id`. Its own module doc (`:22-24`) says it is a "neutral routing primitive"
  and warns against wiring it with hardcoded keyword matchers. It is a **switch, not a spawner** —
  no parallelism, no aggregation, no task lifecycle. Deliberately unwired.
- **`prompts/subagent_system.md`** — a complete, GIAP-flavoured subagent system prompt with
  `task_instructions`, `max_turns`, `subagent_id`, `tool_count` and `available_tools` template
  variables, registered as an override at `giap_prompts.rs:56,97,116`. **GIAP never renders or
  spawns it.** It exists to override a Goose feature GIAP has disabled.
- **`AgentRecipe`** — `user_data/domain/recipe.rs`, `{ id, name, description, yaml, active }`.
  `POST /api/v1/recipes/{name}/run` (`routes.rs:9591`) parses the YAML via
  `goose::recipe::Recipe::from_content`, extracts `prompt` or `instructions`, and runs it through
  the **same chat-stream pipeline as an ordinary user message**. A recipe today is a stored prompt.
- **`ModelRouter`** — `models/services/providers/model_router.rs`, dispatches to `chat` / `think` /
  `task` providers. Its own doc says "all requests currently route to the `chat` provider… `think`
  and `task` are retained for future per-role model assignment but are not auto-selected."
- **`UserSkill`** — named Markdown injected as `extend_system_prompt("skill:<name>", …)` every turn
  (`goose_agent.rs:2337-2345`), deliberately placed outside the KV-cache prefix hash.

No `spawn_subagent`, no task queue, no worker pool, no parallel fan-out, no inter-agent messaging,
no result aggregation exists anywhere in `crates/`.

### 1.2 Goose has real machinery, and GIAP strips it

`goose_agent.rs:2466-2478` removes ten builtins from every session unless the user explicitly added
them: `developer`, `computercontroller`, `extensionmanager`, `todo`, `apps`, `analyze`, **`summon`**,
`summarize`, **`orchestrator`**, `tom`.

What is being stripped:

| Goose surface | What it provides |
|---|---|
| `goose/crates/goose/src/agents/subagent_handler.rs` | `run_subagent_task(SubagentRunParams { config, recipe, task_config, return_last_only, session_id, cancellation_token, on_message, notification_tx })` — a complete child agent loop with cancellation and a notification channel |
| `.../subagent_task_config.rs` | `TaskConfig { provider, model_config, parent_session_id, parent_working_dir, extensions, max_turns }` |
| `.../platform_extensions/summon.rs` | `load` (list/load subrecipes, recipes, agents) and `delegate` (ad-hoc or source-based subagent, `async: true` for background, then `load(taskId)` to collect) |
| `.../platform_extensions/orchestrator.rs` | `list_sessions`, `view_session`, `start_agent`, `send_message`, `interrupt_agent` over Goose's session manager |
| `.../subagent_execution_tool/` | task execution plumbing and `notification_events.rs` |
| `.../recipe/` | full Recipe model, validation, templating, subrecipes |

So the expensive, fiddly part — spawning a child agent with its own conversation, capping its turns,
cancelling it, streaming its progress — already exists and is switched off.

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

Keeping them stripped costs nothing, because the part worth having is `run_subagent_task`, which is
callable directly.

### 3.2 The domain

`pond-core/src/agents/` (new module):

```rust
/// A named, reusable agent persona with hard limits.
pub struct AgentRole {
    pub name: String,
    pub instructions: String,          // from a UserSkill or an AgentRecipe
    pub tool_groups: Vec<String>,      // narrower than the parent's, never wider
    pub model_role: ModelRole,         // Chat | Think | Task — ModelRouter already has these
    pub profile_scope: ProfileScope,   // inherited from the parent, never widened
    pub max_turns: u32,
    pub context_fraction: f32,         // share of the window this role may claim
}

pub struct TaskSpec { pub role: String, pub instructions: String, pub inputs: serde_json::Value }
pub struct TaskRun  { pub id: String, pub status: TaskStatus, pub result: Option<String>, /* … */ }

#[async_trait]
pub trait Orchestrator: Send + Sync {
    async fn spawn(&self, spec: TaskSpec, parent: &SessionRef) -> Result<TaskRun>;
    async fn poll(&self, task_id: &str) -> Result<TaskRun>;
    async fn cancel(&self, task_id: &str) -> Result<()>;
    async fn list(&self, parent: &SessionRef) -> Result<Vec<TaskRun>>;
}
```

`pond-adapters-goose` implements `Orchestrator` over `run_subagent_task`, translating an
`AgentRole` into a `Recipe` plus a `TaskConfig`, and rendering GIAP's existing
`prompts/subagent_system.md` — the prompt that has been sitting unused since it was written.

### 3.3 What is reused rather than rebuilt

| Need | Existing code |
|---|---|
| Role definitions | `AgentRecipe` — already stores Goose YAML, already parsed at `routes.rs:9614` |
| Reusable instruction blocks | `UserSkill` |
| Per-role tool narrowing | `mcp/domain/tool_group.rs` + `services/tool_selection.rs` |
| Per-role model | `ModelRouter`'s `Chat | Think | Task`, currently all aliased to `chat` |
| Cheap in-process routing | `DelegatingAgent`, where a full child agent is overkill |
| Child agent execution | Goose `run_subagent_task` |
| Subagent system prompt | `prompts/subagent_system.md` |
| Concurrency limiting | The `Semaphore` pattern from `schedule_executors.rs` |

The genuinely new code is the domain types, the port, the adapter translation layer, and one MCP
extension.

### 3.4 On-device reality: concurrency 1, and that is fine

One GPU, one resident model. Two subagents on a Jetson do not run in parallel; they interleave
badly, thrash the KV cache, and finish later than if they had run one after another.

So: **default concurrency 1 for `local`/`gguf`; parallelism enabled only for HTTP providers.**

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

Nested delegation is capped at depth 1 in v1. A subagent may not spawn.

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

### 3.7 Streaming

`AgentStreamEvent::SubagentProgress { task_id, role, status, detail }`, fed by
`run_subagent_task`'s `notification_tx`, so the desktop can render the tree instead of a spinner.
Subagent output is **not** persisted into the parent's `session_messages` — only its final result,
as a tool result, which is what keeps the context isolation real.

---

## 4. Phases

- **P1** Domain: `AgentRole`, `TaskSpec`, `TaskRun`, `Orchestrator` port. Roles stored in the
  existing `recipes` table; no new persistence.
- **P2** `GooseOrchestrator` over `run_subagent_task`; render `subagent_system.md`; translate role →
  `Recipe` + `TaskConfig`. Concurrency 1 on local.
- **P3** Scope inheritance: tool groups narrowed per role, profile scope inherited and never widened,
  draft gate enforced inside subagents.
- **P4** Budget: `context_fraction` through the governor; parent budget shrinks while a child is
  live; depth capped at 1.
- **P5** `giap-orchestrator` MCP extension + `ext_orchestrator_enabled`; update the extension count
  in `CLAUDE.md`.
- **P6** `SubagentProgress` streaming + desktop rendering.
- **P7** Per-role model assignment through `ModelRouter`'s existing `Think`/`Task` slots.
- **P8** Background tasks for HTTP providers only, with `check_task`.

---

## 5. Invariants

1. A subagent's scope is a subset of its parent's. Never wider — not tools, not profile, not
   network.
2. Goose's `summon`, `orchestrator` and `todo` stay stripped.
3. Concurrency is 1 on `local`/`gguf` until measurement says otherwise.
4. Subagent conversations never enter the parent's `session_messages`; only results do.
5. Every spawn is cancellable, and cancelling a parent cancels its children.
6. Depth is capped. A recursive delegation loop on a home server is a fire.

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

- **Unit** — scope inheritance: a role requesting a tool group its parent lacks gets the parent's
  set; a role requesting a wider profile scope is denied and audited.
- **Budget** — with a child live, the parent's history budget shrinks by `context_fraction`; assert
  the sum never exceeds `usable_prompt_tokens`.
- **Integration** — delegate a research task on the Orin, assert the parent's context grows by the
  result only, and compare parent context growth against running the same task inline. That
  comparison is the entire on-device justification for this workstream and should be recorded as a
  measurement in this document once taken.
- **Cancellation** — cancel a parent mid-delegation; assert the child terminates and no orphan task
  remains.
- **Canary** — a test reading the fork's builtin list so that a Goose sync adding a new
  orchestration builtin fails loudly rather than silently re-enabling it, in the spirit of
  `goose_cap_message_is_still_verbatim`.
