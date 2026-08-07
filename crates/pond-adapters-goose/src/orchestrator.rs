//! Adapter: `GooseOrchestrator` — PAI-6 P2.
//!
//! `pond-core` decided *what may run and under what limits* (PAI-6 P1's
//! [`TaskSpec`]). This file is the other half: *how* a child agent is actually
//! executed on Goose. Nothing here makes an authorisation decision — every
//! narrowing it applies is one it was handed, plus defence in depth that can
//! only ever refuse.
//!
//! # Why this owns the child loop instead of calling `run_subagent_task`
//!
//! The phase text said "`GooseOrchestrator` over `run_subagent_task`". That does
//! not compile. `goose/crates/goose/src/agents/mod.rs` declares
//! `pub(crate) mod subagent_handler;` and re-exports exactly
//! `SUBAGENT_TOOL_REQUEST_TYPE` and `TaskConfig`, so `run_subagent_task`,
//! `SubagentRunParams` and `OnMessageCallback` are crate-private to `goose` and
//! unreachable from here. The only caller in the whole Goose workspace is
//! `platform_extensions/summon.rs` — the extension GIAP strips.
//!
//! The two routes were a sixth GIAP fork patch (two `pub(crate)` → `pub`, staged
//! on `jarida-io/Goose` with a CI fork-branch-tip bump) or owning the ~120 lines
//! of [`GooseAdapter::run_child_agent`]. This took the second, for a reason that
//! is not "avoid the patch": `run_subagent_task` calls `build_subagent_prompt`,
//! which unconditionally renders Goose's own `subagent_system.md` ("You are a
//! specialized subagent within the goose AI framework, created by AAIF") and
//! then calls `override_system_prompt` on an `Agent` it constructs internally
//! and never returns. There is no seam. Patching visibility would have bought a
//! callable function whose child still introduces itself as somebody else's
//! product, and `GiapProviderShim` would not correct it — its
//! `GOOSE_DEFAULT_MARKER` does not appear in that template.
//!
//! Owning the loop is what makes the phase's stated respecification possible at
//! all: the child's system prompt is built through the LIVE path,
//! `build_prompt_partition`, exactly like the parent's.
//!
//! # What is enforced here, and where
//!
//! | Invariant | Mechanism |
//! |---|---|
//! | 1 — never wider | [`child_extensions`] intersects THREE independent sets and populates `available_tools` explicitly; an extension whose tool list comes out empty is dropped, never passed as `vec![]` |
//! | 2 — `summon`/`orchestrator`/`todo` stay stripped | [`GOOSE_STRIPPED_BUILTINS`] is now the single source for `goose_agent.rs`'s strip list, this file's plan-time refusal, and the post-run audit in [`stripped_builtins_present`] |
//! | 3 — concurrency 1 on this device | one [`Semaphore`], acquired with [`subagent_permits`] permits, on the only path that can start a child |
//! | 4 — child turns never reach the parent's history | the drain loop keeps only `as_concat_text()` of the last assistant message, and [`ChildRunner::release`] deletes the child's engine session afterwards |
//! | 5 — cancellable, and a parent cancels its children | one [`CancellationToken`] per run in the registry, plus `cancel_children_of` |
//! | 6 — depth capped | structural: `giap-orchestrator` is not something a child can be given, because [`child_extensions`] only ever emits what the PARENT already had loaded and P1 refuses at the cap regardless |
//!
//! # What this deliberately does not do
//!
//! It does not stream. `spawn` runs the child to completion and returns a
//! terminal [`TaskRun`]; `poll` exists so P8 can add a background path without
//! changing the port. Progress frames are P6, per-role models are P7.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::mcp::domain::tool_group::TOOL_NAME_SEPARATOR;
use pond_core::shared::domain::orchestration::{
    max_concurrent_subagents, TaskRun, TaskSpec, TaskStatus, REMOTE_SUBAGENT_CONCURRENCY,
};
use pond_core::shared::ports::orchestrator::Orchestrator;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use goose::agents::ExtensionConfig;

/// The ten Goose builtins GIAP removes from every session.
///
/// **This is the only copy.** `goose_agent.rs` used to carry two hardcoded
/// lists of these names — `strip_list`, which is the guard, and the `is_builtin`
/// closure, which is a prompt-rendering filter — and only one of them enforced
/// anything, so a canary pinning one proved nothing about the other. Both now
/// read this constant, and PAI-6's plan builder refuses on it as well, so
/// invariant 2 has one thing to break rather than three things to keep in step.
///
/// `is_builtin` additionally treats `default` and `suggestions` as builtins.
/// Those two are not stripped — they are Goose plumbing that never carries
/// tools — so they stay at that call site rather than widening this list into
/// something the strip loop would try to remove.
pub const GOOSE_STRIPPED_BUILTINS: [&str; 10] = [
    "developer",
    "computercontroller",
    "extensionmanager",
    "todo",
    "apps",
    "analyze",
    "summon",
    "summarize",
    "orchestrator",
    "tom",
];

/// The literal string Goose yields instead of an error when a child exhausts
/// `max_turns` (`agent.rs`, `MAX_TURNS_MESSAGE` — a private `const`, so it has
/// to be mirrored).
///
/// `agent.rs`'s reply loop does `if turns_taken > max_turns { last_assistant_text
/// = MAX_TURNS_MESSAGE; yield; break }`. The run returns `Ok`, so an orchestrator
/// that only inspects `Result` reports a blown budget as an answer. Pinned by
/// `the_goose_turn_cap_message_is_still_verbatim`, which reads the submodule.
pub const GOOSE_MAX_TURNS_MESSAGE: &str = "I've reached the maximum number of actions I can do without user input. Would you like me to continue?";

/// Total permits in the subagent semaphore.
///
/// Sized to the *widest* concurrency any provider is allowed, so a single
/// semaphore can express every provider's limit as "how many permits does one
/// child take". See [`subagent_permits`].
pub const SUBAGENT_PERMITS: usize = REMOTE_SUBAGENT_CONCURRENCY;

/// Terminal runs retained for [`Orchestrator::poll`] / [`Orchestrator::list`].
///
/// Bounded because the registry is process-lifetime state on a home server.
/// Eviction only ever removes a run that has already finished — the
/// `ShimControls` session map next door evicts oldest-first regardless, which
/// means sixty-four children can silently evict a live parent's allow-set, and
/// repeating that shape here would let a cancel arrive for a task the registry
/// had forgotten was running.
pub const MAX_TRACKED_TASKS: usize = 64;

/// How many of [`SUBAGENT_PERMITS`] one child must hold, given the provider.
///
/// The indirection buys one thing worth having: the concurrency limit is
/// enforced by a permit count on the only path that can start a child, not by a
/// convention that a later caller could forget. There is no `spawn` that does
/// not acquire.
///
/// **The predicate is `max_concurrent_subagents`, which is written against
/// `runs_on_this_device`** — `local`, `gguf`, `ollama` and `llamafile`. Ollama
/// and llamafile speak HTTP, but on a GIAP pond they speak it to `127.0.0.1`,
/// which is the same GPU the parent's next turn needs. PAI-4 P2 already had to
/// fix the narrow `local`/`gguf` reading once.
///
/// `div_ceil` rather than `/` so an awkward ratio errs toward LESS concurrency.
/// With `SUBAGENT_PERMITS = 3`: on-device asks for 3 of 3 (concurrency 1),
/// remote asks for 1 of 3 (concurrency 3).
pub fn subagent_permits(provider: &str) -> u32 {
    let concurrent = max_concurrent_subagents(provider).max(1);
    SUBAGENT_PERMITS.div_ceil(concurrent) as u32
}

// ── What the engine offers, and what we decide to run ───────────────────────

/// The parent's current engine surface, as the plan builder sees it.
///
/// Built by [`ChildRunner::environment`] from the PARENT's live Goose session,
/// which is what makes "the child's tools are a subset of the parent's" true by
/// construction rather than by comparison: a tool the parent does not have
/// loaded cannot appear here, so it cannot appear in a plan.
#[derive(Debug, Clone, Default)]
pub struct ChildEnvironment {
    /// `settings.chat_provider`, for the concurrency permit.
    pub provider_name: String,
    /// The parent's own static system prefix, as `build_prompt_partition`
    /// produced it. The child's prompt is this plus a subagent envelope.
    pub base_system_prefix: String,
    /// Extension name -> UNPREFIXED tool names, as currently loaded on the
    /// parent. Unprefixed because that is what Goose's
    /// `ExtensionConfig::is_tool_available` matches against — it is checked
    /// against `resolved.actual_tool_name` inside `dispatch_tool_call`, not
    /// against the `ext__tool` name the model sees.
    pub parent_tools: BTreeMap<String, BTreeSet<String>>,
}

/// Everything needed to run one child, with every decision already made.
///
/// This is the security artifact of the phase. It is built by
/// [`build_child_plan`] from a [`TaskSpec`] and a [`ChildEnvironment`], and the
/// runner does exactly what it says — the runner makes no choices about scope,
/// tools or turns.
#[derive(Debug, Clone)]
pub struct ChildPlan {
    pub task_id: String,
    pub role: String,
    pub parent_session_id: String,
    pub child_session_id: String,
    pub system_prompt: String,
    pub user_message: String,
    pub extensions: Vec<ExtensionConfig>,
    pub max_turns: u32,
}

/// What came back from one child run, before it is classified.
#[derive(Debug, Clone, Default)]
pub struct ChildOutcome {
    /// `as_concat_text()` of the last assistant message. Text only — Goose's
    /// `as_text()` returns `None` for `MessageContent::Thinking`, so a child's
    /// reasoning cannot reach the parent through this field. PAI-5's gate lives
    /// at the `GooseAdapter` producer, which this path does not go through.
    pub last_text: Option<String>,
    /// Assistant messages seen. Goose increments `turns_taken` once per
    /// productive turn and trips at `turns_taken > max_turns`, so this crossing
    /// `max_turns` is the same event as the sentinel below.
    pub assistant_turns: u32,
    /// What the child agent actually had loaded when it ran. Audited against
    /// [`GOOSE_STRIPPED_BUILTINS`] after the fact, because
    /// `Agent::add_extension` is not the only way an extension can arrive.
    pub loaded_extensions: BTreeSet<String>,
}

/// Why a plan could not be built. Every variant is a refusal to run, never a
/// substitution — the widening failure path in this workstream is always
/// "carry on with a default".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRefused {
    ForbiddenExtension {
        task_id: String,
        role: String,
        extension: String,
    },
}

impl std::fmt::Display for PlanRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanRefused::ForbiddenExtension {
                task_id,
                role,
                extension,
            } => write!(
                f,
                "task {task_id} for role `{role}` was authorised for `{extension}`, which is one \
                 of the Goose builtins GIAP strips from every session - refusing to hand it to a \
                 subagent"
            ),
        }
    }
}

impl std::error::Error for PlanRefused {}

/// Build the child's extension list.
///
/// Two narrowings and one refusal:
///
/// 1. `env.parent_tools` — the loop's domain is what the parent's engine
///    session ACTUALLY has loaded, so a tool the parent does not hold cannot be
///    named in a plan. This is the structural half of invariant 1.
/// 2. `spec.grants_tool` — P1's own predicate, applied per tool. It denies an
///    unknown prefix and an empty set, so "unknown means no".
/// 3. [`GOOSE_STRIPPED_BUILTINS`] in the spec — refused outright rather than
///    filtered, see [`PlanRefused`]. A spec naming `summon` means something
///    upstream is broken and silence would hide it.
///
/// And `available_tools` is populated with real names. Goose's predicate is
/// `available_tools.is_empty() || available_tools.contains(&tool_name)`, so an
/// empty vector means **all tools of that extension**, which is why
/// `GooseAdapter::builtin_extension_config` must never be reused here: it passes
/// `vec![]`. An extension whose filtered tool list is empty is dropped rather
/// than emitted with an empty allowlist.
pub fn child_extensions(
    spec: &TaskSpec,
    parent_tools: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Vec<ExtensionConfig>, PlanRefused> {
    for extension in spec.tool_groups() {
        if GOOSE_STRIPPED_BUILTINS.contains(&extension.as_str()) {
            return Err(PlanRefused::ForbiddenExtension {
                task_id: spec.id().to_string(),
                role: spec.role().to_string(),
                extension: extension.clone(),
            });
        }
    }

    let mut configs = Vec::new();
    for (extension, tools) in parent_tools {
        // Narrowing 1 and 2 at once, per tool, through P1's own predicate.
        //
        // I wrote this twice — an extension-level `spec.tool_groups().contains`
        // AND this — and then removed the first, because it is redundant:
        // `group_of_tool("{ext}__{tool}")` returns `{ext}` for every name built
        // this way, so an extension the spec does not name produces an empty
        // `granted` and is dropped two lines down. Deleting the extension-level
        // check left every test green, which is this programme's recorded shape
        // for a guard that is really being satisfied by something else. One
        // mechanism, one guard.
        //
        // It is also what keeps a parent whose strip FAILED from leaking its
        // builtins into a child: the strip is a best-effort
        // `remove_extension(...).ok()`, so a parent really can be listing
        // `developer__shell` here, and nothing authorised it.
        let granted: Vec<String> = tools
            .iter()
            .filter(|tool| spec.grants_tool(&format!("{extension}{TOOL_NAME_SEPARATOR}{tool}")))
            .cloned()
            .collect();
        // Never `vec![]`: an empty allowlist is Goose's "all tools".
        if granted.is_empty() {
            continue;
        }
        configs.push(ExtensionConfig::Builtin {
            name: extension.clone(),
            description: String::new(),
            display_name: None,
            timeout: Some(600),
            bundled: Some(false),
            available_tools: granted,
        });
    }
    Ok(configs)
}

/// Which of the ten stripped builtins a child ended up with. Empty is the only
/// acceptable answer.
pub fn stripped_builtins_present(loaded: &BTreeSet<String>) -> Vec<String> {
    GOOSE_STRIPPED_BUILTINS
        .iter()
        .filter(|name| loaded.contains(**name))
        .map(|name| (*name).to_string())
        .collect()
}

/// The subagent framing, appended to the parent's own static prefix.
///
/// Deliberately GIAP's words. Goose's `subagent_system.md` opens "You are a
/// specialized subagent within the goose AI framework, created by AAIF (Agentic
/// AI Foundation)" and carries coding-agent tool-efficiency rules; a 2-4B
/// on-device model reads that and acts on it.
fn subagent_envelope(spec: &TaskSpec, tools: &[String]) -> String {
    let mut envelope = String::with_capacity(640);
    envelope.push_str("\n\n# Delegated task\n\n");
    envelope.push_str(
        "You are running as a helper for the assistant above, on ONE narrow task. \
         You are not talking to the user and they will not see this conversation - \
         only your final answer is passed back.\n\n",
    );
    envelope.push_str("Rules for this run:\n");
    envelope.push_str(
        "- Answer the task and stop. Do not ask follow-up questions; there is nobody to answer them.\n",
    );
    envelope.push_str(
        "- Your last message IS the answer. Make it complete on its own, in a few sentences.\n",
    );
    envelope.push_str(&format!(
        "- You have at most {} turns. Spend them on the task.\n",
        spec.max_turns()
    ));
    envelope.push_str("- You cannot delegate. There is no one below you.\n");
    if tools.is_empty() {
        envelope.push_str("- You have no tools on this run. Answer from what you are given.\n");
    } else {
        envelope.push_str(&format!(
            "- Your only tools are: {}. Nothing else is available to you.\n",
            tools.join(", ")
        ));
    }
    envelope.push_str("\n## Your role\n\n");
    envelope.push_str(spec.role());
    envelope.push('\n');
    envelope
}

/// The child's opening user message: the task, plus any structured inputs.
fn child_user_message(spec: &TaskSpec) -> String {
    let mut message = spec.instructions().to_string();
    if !spec.inputs().is_null() {
        message.push_str("\n\nInputs:\n");
        message.push_str(
            &serde_json::to_string_pretty(spec.inputs())
                .unwrap_or_else(|_| spec.inputs().to_string()),
        );
    }
    message
}

/// Turn an authorised [`TaskSpec`] into an executable [`ChildPlan`].
///
/// Pure, so the security decisions in it can be tested without an engine, a
/// provider or a session.
pub fn build_child_plan(
    spec: &TaskSpec,
    child_session_id: &str,
    env: &ChildEnvironment,
) -> Result<ChildPlan, PlanRefused> {
    let extensions = child_extensions(spec, &env.parent_tools)?;
    let tool_names: Vec<String> = extensions
        .iter()
        .flat_map(|config| match config {
            ExtensionConfig::Builtin {
                name,
                available_tools,
                ..
            } => available_tools
                .iter()
                .map(|tool| format!("{name}{TOOL_NAME_SEPARATOR}{tool}"))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();

    let mut system_prompt = env.base_system_prefix.clone();
    system_prompt.push_str(&subagent_envelope(spec, &tool_names));

    Ok(ChildPlan {
        task_id: spec.id().to_string(),
        role: spec.role().to_string(),
        parent_session_id: spec.parent_session_id().to_string(),
        child_session_id: child_session_id.to_string(),
        system_prompt,
        user_message: child_user_message(spec),
        extensions,
        max_turns: spec.max_turns(),
    })
}

/// Decide what a finished child run actually was.
///
/// Goose returns `Ok` for all three of "answered", "cancelled" and "ran out of
/// turns" — the reply loop simply `break`s in the first two cases — so this is
/// the only thing standing between a blown turn budget and the user being told
/// it was an answer.
///
/// Order matters. Cancellation is checked first because a cancelled run may
/// also have crossed the turn count, and "the parent stopped it" is the more
/// accurate account of what happened.
pub fn classify_outcome(
    cancelled: bool,
    assistant_turns: u32,
    max_turns: u32,
    last_text: Option<&str>,
) -> (TaskStatus, Option<String>) {
    if cancelled {
        return (TaskStatus::Cancelled, None);
    }
    let text = last_text.map(str::trim).unwrap_or("");
    if text == GOOSE_MAX_TURNS_MESSAGE || assistant_turns > max_turns {
        return (TaskStatus::TurnBudgetExhausted, None);
    }
    if text.is_empty() {
        return (TaskStatus::Failed, None);
    }
    (TaskStatus::Completed, Some(text.to_string()))
}

// ── The engine seam ─────────────────────────────────────────────────────────

/// How a child agent is actually driven.
///
/// Split out from [`GooseOrchestrator`] so that the semaphore, the registry,
/// the cancellation bookkeeping and the plan-building above can be tested
/// against a fake engine — the *plan* the fake receives is the real one,
/// produced by production code, which is the thing worth asserting on.
///
/// **No default method bodies.** A defaulted trait method is one of this
/// programme's recorded vacuity shapes: deleting a real override leaves the
/// tree green while the feature silently stops working.
#[async_trait]
pub trait ChildRunner: Send + Sync {
    /// What the parent's engine session currently offers.
    async fn environment(&self, parent_session_id: &str) -> Result<ChildEnvironment>;

    /// Create the child's engine session and return its id.
    ///
    /// Separate from [`run`](Self::run) because Goose mints the id, and the
    /// plan cannot be built without it: `Agent::update_provider` ends in a
    /// `session_manager.update(...).apply()` that errors if the row does not
    /// exist, which surfaces as "Failed to set provider on sub agent" and reads
    /// like a provider fault.
    async fn open_child_session(&self, plan_role: &str) -> Result<String>;

    /// Run the plan to completion, or until `cancel` trips.
    async fn run(&self, plan: ChildPlan, cancel: CancellationToken) -> Result<ChildOutcome>;

    /// Drop the child's engine session.
    ///
    /// PAI-6 invariant 4 says subagent conversations never enter the parent's
    /// `session_messages`, which is true by construction — `ChatService` is the
    /// sole writer of that table and nothing here touches it. What was NOT true
    /// is "not written down": `Agent::reply` persists every child message into
    /// Goose's own `sessions.db` under the child's id, where nothing in GIAP
    /// will ever read it or clean it up. Releasing it here closes both the leak
    /// and the at-rest half of the invariant.
    async fn release(&self, child_session_id: &str);
}

// ── The registry ────────────────────────────────────────────────────────────

struct TaskEntry {
    run: TaskRun,
    cancel: CancellationToken,
}

#[derive(Default)]
struct TaskRegistry {
    tasks: HashMap<String, TaskEntry>,
}

impl TaskRegistry {
    fn insert(&mut self, run: TaskRun, cancel: CancellationToken) {
        if self.tasks.len() >= MAX_TRACKED_TASKS {
            // Only ever evict something that has finished. A running task whose
            // entry is gone cannot be cancelled and cannot be polled, which is
            // the failure this cap exists to bound rather than cause.
            let oldest_terminal = self
                .tasks
                .values()
                .filter(|entry| entry.run.status.is_terminal())
                .min_by_key(|entry| entry.run.started_at)
                .map(|entry| entry.run.id.clone());
            if let Some(id) = oldest_terminal {
                self.tasks.remove(&id);
            }
        }
        self.tasks.insert(run.id.clone(), TaskEntry { run, cancel });
    }

    fn finish(
        &mut self,
        task_id: &str,
        status: TaskStatus,
        result: Option<String>,
        error: Option<String>,
    ) {
        if let Some(entry) = self.tasks.get_mut(task_id) {
            entry.run.status = status;
            entry.run.result = result;
            entry.run.error = error;
            entry.run.finished_at = Some(chrono::Utc::now());
        }
    }
}

// ── The orchestrator ────────────────────────────────────────────────────────

/// Adapter: runs [`TaskSpec`]s as Goose child agents.
pub struct GooseOrchestrator {
    runner: Arc<dyn ChildRunner>,
    /// One semaphore for every child on this pond. Invariant 3 is a property of
    /// the device, not of a session, so the limit is process-wide.
    permits: Arc<Semaphore>,
    tasks: Mutex<TaskRegistry>,
}

impl GooseOrchestrator {
    pub fn new(runner: Arc<dyn ChildRunner>) -> Self {
        Self {
            runner,
            permits: Arc::new(Semaphore::new(SUBAGENT_PERMITS)),
            tasks: Mutex::new(TaskRegistry::default()),
        }
    }

    fn with_registry<T>(&self, f: impl FnOnce(&mut TaskRegistry) -> T) -> T {
        let mut guard = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }
}

#[async_trait]
impl Orchestrator for GooseOrchestrator {
    async fn spawn(&self, spec: TaskSpec) -> Result<TaskRun> {
        let env = self.runner.environment(spec.parent_session_id()).await?;
        let child_session_id = self.runner.open_child_session(spec.role()).await?;
        let plan = match build_child_plan(&spec, &child_session_id, &env) {
            Ok(plan) => plan,
            Err(refused) => {
                self.runner.release(&child_session_id).await;
                return Err(anyhow!(refused));
            }
        };

        let cancel = CancellationToken::new();
        let mut run = TaskRun::started(&spec, chrono::Utc::now());
        self.with_registry(|registry| registry.insert(run.clone(), cancel.clone()));

        // Invariant 3, on the only path that can start a child. The permit is
        // acquired BEFORE the engine is touched and held until the run ends, so
        // there is no window in which two on-device children are both replying.
        let needed = subagent_permits(&env.provider_name);
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            acquired = self.permits.clone().acquire_many_owned(needed) => Some(acquired?),
        };
        let Some(_permit) = permit else {
            // Cancelled while queued. Nothing ran, so there is nothing to
            // classify -- but the status still has to say Cancelled rather than
            // look like a completion with no result.
            self.runner.release(&child_session_id).await;
            self.with_registry(|registry| {
                registry.finish(&run.id, TaskStatus::Cancelled, None, None)
            });
            run.status = TaskStatus::Cancelled;
            run.finished_at = Some(chrono::Utc::now());
            return Ok(run);
        };

        let max_turns = plan.max_turns;
        let outcome = self.runner.run(plan, cancel.clone()).await;
        self.runner.release(&child_session_id).await;

        let (status, result, error) = match outcome {
            Ok(outcome) => {
                // Invariant 2, audited after the fact. The plan builder refuses
                // to ASK for a stripped builtin; this asks what the child
                // actually ended up holding, because `add_extension` is not the
                // only way one can arrive (a Goose sync could re-arm a
                // `default_enabled` platform extension, which is exactly what
                // `EnabledExtensionsState::extensions_or_default` does).
                let smuggled = stripped_builtins_present(&outcome.loaded_extensions);
                if !smuggled.is_empty() {
                    tracing::error!(
                        task_id = %run.id,
                        role = %run.role,
                        extensions = %smuggled.join(", "),
                        "subagent loaded Goose builtins GIAP strips - discarding its result"
                    );
                    (
                        TaskStatus::Failed,
                        None,
                        Some(format!(
                            "subagent loaded stripped Goose builtins: {}",
                            smuggled.join(", ")
                        )),
                    )
                } else {
                    // The token is re-checked HERE, after the await, because
                    // Goose's reply loop breaks out and returns Ok on
                    // cancellation - the return type cannot tell us.
                    let (status, result) = classify_outcome(
                        cancel.is_cancelled(),
                        outcome.assistant_turns,
                        max_turns,
                        outcome.last_text.as_deref(),
                    );
                    let error = match status {
                        TaskStatus::Failed => Some("subagent produced no answer".to_string()),
                        _ => None,
                    };
                    (status, result, error)
                }
            }
            Err(e) => (TaskStatus::Failed, None, Some(e.to_string())),
        };

        self.with_registry(|registry| {
            registry.finish(&run.id, status, result.clone(), error.clone())
        });
        run.status = status;
        run.result = result;
        run.error = error;
        run.finished_at = Some(chrono::Utc::now());
        Ok(run)
    }

    async fn poll(&self, task_id: &str) -> Result<Option<TaskRun>> {
        Ok(self
            .with_registry(|registry| registry.tasks.get(task_id).map(|entry| entry.run.clone())))
    }

    async fn cancel(&self, task_id: &str) -> Result<()> {
        self.with_registry(|registry| {
            if let Some(entry) = registry.tasks.get(task_id) {
                entry.cancel.cancel();
            }
        });
        Ok(())
    }

    async fn list(&self, parent_session_id: &str) -> Result<Vec<TaskRun>> {
        Ok(self.with_registry(|registry| {
            let mut runs: Vec<TaskRun> = registry
                .tasks
                .values()
                .filter(|entry| entry.run.parent_session_id == parent_session_id)
                .map(|entry| entry.run.clone())
                .collect();
            runs.sort_by_key(|run| run.started_at);
            runs
        }))
    }

    async fn cancel_children_of(&self, parent_session_id: &str) -> Result<usize> {
        Ok(self.with_registry(|registry| {
            let mut stopped = 0usize;
            for entry in registry.tasks.values() {
                if entry.run.parent_session_id == parent_session_id
                    && !entry.run.status.is_terminal()
                {
                    entry.cancel.cancel();
                    stopped += 1;
                }
            }
            stopped
        }))
    }
}

// ── The production runner ───────────────────────────────────────────────────

/// [`ChildRunner`] over the live [`GooseAdapter`].
///
/// A thin delegation on purpose: the four methods need `agent`,
/// `session_manager`, `current_provider`, `settings_repo` and `template_repo`,
/// which are private fields, so the bodies live next to them in
/// `goose_agent.rs`. What lives here is the shape the orchestrator depends on.
pub struct GooseChildRunner {
    adapter: Arc<crate::goose_agent::GooseAdapter>,
}

impl GooseChildRunner {
    pub fn new(adapter: Arc<crate::goose_agent::GooseAdapter>) -> Self {
        Self { adapter }
    }
}

#[async_trait]
impl ChildRunner for GooseChildRunner {
    async fn environment(&self, parent_session_id: &str) -> Result<ChildEnvironment> {
        self.adapter.child_environment(parent_session_id).await
    }

    async fn open_child_session(&self, plan_role: &str) -> Result<String> {
        self.adapter.open_child_session(plan_role).await
    }

    async fn run(&self, plan: ChildPlan, cancel: CancellationToken) -> Result<ChildOutcome> {
        self.adapter.run_child_agent(plan, cancel).await
    }

    async fn release(&self, child_session_id: &str) {
        self.adapter.release_child_session(child_session_id).await;
    }
}

#[cfg(test)]
mod tests;
