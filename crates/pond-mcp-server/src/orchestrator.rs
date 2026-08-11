//! Orchestrator MCP Server — the `delegate` tool (PAI-6 P5).
//!
//! One tool. It is the first and only caller of
//! [`Orchestrator::spawn`](pond_core::shared::ports::orchestrator::Orchestrator::spawn),
//! which means it is what makes PAI-6 P1's authority, P2's child loop and P3's
//! registry do anything at all. Until this existed they were three correct
//! mechanisms with no path into them.
//!
//! # The resolution, which is the whole substance
//!
//! An MCP tool handler has no access to the adapter's per-turn locals. What it
//! has is the caller's ENGINE session id, stamped into `_meta` by the engine
//! itself (see [`crate::session_meta`]) and un-forgeable by the model. PAI-6 P3
//! put a registry behind exactly that key, so:
//!
//! 1. `_meta` gives the engine session id.
//! 2. [`TurnAuthorityRegistry::authority_for_engine_session`] gives the live
//!    turn's [`DelegationAuthority`], or `None`.
//! 3. `None` means refuse. **Four different inputs produce it** — a session GIAP
//!    never chatted in, a subagent's own session, a turn that has already ended,
//!    and a call carrying no `_meta` at all — and all four get the same refusal
//!    text, because a caller that could tell them apart would eventually treat
//!    one of them as benign.
//! 4. The role comes from an ordinary `agent_recipes` row through
//!    [`AgentRole::from_recipe_yaml`]. `Ok(None)` is an ordinary routine and
//!    `Err` is a role block that will not parse; **both refuse**, and the `Err`
//!    direction is the one that matters. Substituting a default role on the
//!    exact input a human got wrong would hand a child whatever the default tool
//!    set is.
//! 5. [`DelegationAuthority::delegate`] does the narrowing. Nothing here decides
//!    what a child may do; this module decides only whether to ask.
//!
//! # Why the deps arrive through a global installer
//!
//! `pond-mcp-server` is built BEFORE `pond-adapters-goose` in the dependency
//! order, so the orchestrator — which is a Goose adapter — cannot be a direct
//! dependency. [`init_orchestrator_deps`] is the same shape as
//! [`crate::toolkit::init_toolkit_deps`], which exists for the same reason and
//! for the sharper version of it: the implementor is the agent adapter, and the
//! adapter does not exist until after extension registration has run.
//!
//! # What this module deliberately does NOT do
//!
//! Decide whether a child should be offered this tool. That is answered by
//! WITHHOLDING, in [`groups_denied_to_subagents`], for the reason PAI-6 P3
//! records: a tool a small model cannot see costs nothing, and a tool it can see
//! but whose every call is refused costs turns off a budget of six. The depth
//! check below is the enforcement; the denylist is what stops the question being
//! asked.
//!
//! [`TurnAuthorityRegistry::authority_for_engine_session`]: pond_core::shared::services::turn_authority::TurnAuthorityRegistry::authority_for_engine_session
//! [`groups_denied_to_subagents`]: pond_core::mcp::domain::tool_group::groups_denied_to_subagents

use pond_core::mcp::domain::tool_group::ORCHESTRATOR_EXTENSION;
use pond_core::shared::domain::orchestration::{
    AgentRole, DelegationAuthority, RoleError, TaskRequest, TaskRun, TaskSpec, TaskStatus,
};
use pond_core::shared::ports::orchestrator::Orchestrator;
use pond_core::shared::services::turn_authority::TurnAuthorityRegistry;
use pond_core::user_data::ports::recipe::AgentRecipeRepository;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

// ── Parameter struct ────────────────────────────────────────────────────────

/// The wire shape of a `delegate` call.
///
/// **This is a schema carrier, not the parser.** The authority on what a
/// delegation may say is [`TaskRequest`], which is `deny_unknown_fields` and has
/// no scope, tool, depth or session field precisely so that a model trying to
/// widen its own authority gets an error it can read instead of a silently
/// dropped field. [`into_request`] rebuilds the payload — declared fields AND
/// the extras bag — and hands the whole thing to `TaskRequest` to parse, so that
/// property is preserved rather than re-implemented here.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DelegateParams {
    /// The exact name of the saved role to run.
    pub role: Option<String>,
    /// What this particular agent is being asked to do, in plain words.
    pub instructions: Option<String>,
    /// Optional structured inputs, passed through to the agent unchanged.
    pub inputs: Option<serde_json::Value>,
    /// Run without waiting for the answer. Only on a pond whose model runs
    /// somewhere else; on this device it is refused. Default false.
    ///
    /// **Typed as a `Value` and schema'd as a boolean, on purpose.** Declaring
    /// it `Option<bool>` would make `{"background": "true"}` — which small
    /// models emit constantly — an rmcp deserialization failure of the whole
    /// call, and an MCP protocol error is precisely what this module avoids: it
    /// makes a small model retry the identical bad call. As a `Value` every
    /// spelling reaches [`into_request`], the common ones are recovered like the
    /// aliases beside them, and anything else is refused by [`TaskRequest`] with
    /// a sentence naming the field.
    #[serde(default)]
    #[schemars(with = "Option<bool>")]
    pub background: Option<serde_json::Value>,
    /// Catch-all for unexpected fields.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Argument names a small model invents for `role` and `instructions`.
///
/// Recovered because the alternative is a wasted turn off a budget of six.
/// **Every entry here must be a synonym for one of the two fields a caller is
/// allowed to supply.** Nothing that names a scope, a tool set, a depth or a
/// session may ever appear: those are not fields a caller has, and adding one
/// here would turn `TaskRequest`'s `deny_unknown_fields` from a boundary into a
/// spelling check. `a_widening_field_is_refused_under_every_spelling` is the
/// guard.
const ROLE_ALIASES: [&str; 4] = ["name", "agent", "role_name", "agent_name"];
const INSTRUCTION_ALIASES: [&str; 4] = ["task", "instruction", "prompt", "request"];

/// Rebuild the payload and let [`TaskRequest`] parse it.
///
/// Aliases are MOVED out of the extras bag, so anything left there is genuinely
/// unrecognised and reaches `deny_unknown_fields`.
fn into_request(mut params: DelegateParams) -> Result<TaskRequest, Refusal> {
    let mut obj = serde_json::Map::new();

    let role = params
        .role
        .take()
        .or_else(|| take_alias(&mut params.extra, &ROLE_ALIASES));
    if let Some(role) = role {
        obj.insert("role".into(), serde_json::Value::String(role));
    }
    let instructions = params
        .instructions
        .take()
        .or_else(|| take_alias(&mut params.extra, &INSTRUCTION_ALIASES));
    if let Some(instructions) = instructions {
        obj.insert(
            "instructions".into(),
            serde_json::Value::String(instructions),
        );
    }
    if let Some(inputs) = params.inputs.take() {
        obj.insert("inputs".into(), inputs);
    }
    if let Some(background) = params.background.take() {
        // Refused HERE rather than by `TaskRequest`, because serde's own type
        // error for a struct field does not name the field: "invalid type:
        // string, expected a boolean" tells a 2-4B model nothing it can act on.
        let Some(background) = coerce_bool(&background) else {
            return Err(Refusal::Malformed(format!(
                "`background` must be true or false, not `{background}`"
            )));
        };
        obj.insert("background".into(), serde_json::Value::Bool(background));
    }
    // Everything the model sent that is not one of the three. Left in so
    // `TaskRequest` refuses it rather than this function dropping it.
    for (k, v) in params.extra {
        obj.insert(k, v);
    }

    serde_json::from_value::<TaskRequest>(serde_json::Value::Object(obj))
        .map_err(|e| Refusal::Malformed(e.to_string()))
}

/// The wire shape of a `check_task` call — PAI-6 P8.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CheckTaskParams {
    /// The id you were given when you started the task.
    pub task_id: Option<String>,
    /// Catch-all, so an invented argument name can be recovered rather than
    /// costing a turn.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Argument names a small model invents for `task_id`.
///
/// Unlike [`ROLE_ALIASES`] there is nothing here that could widen anything —
/// the id names a run, and [`authorise_task`] decides whether the caller may see
/// it whatever the id turned out to be.
const TASK_ID_ALIASES: [&str; 4] = ["id", "task", "taskId", "task-id"];

/// Pull the task id out of a `check_task` call, or refuse.
///
/// A blank or missing id is refused rather than defaulted to anything — there is
/// no sensible "the last one", and inventing one would answer a question the
/// caller did not ask.
pub fn check_task_id(mut params: CheckTaskParams) -> Result<String, Refusal> {
    let id = params
        .task_id
        .take()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .or_else(|| take_alias(&mut params.extra, &TASK_ID_ALIASES));
    id.ok_or_else(|| {
        Refusal::Malformed(
            "check_task needs the `task_id` you were given when the task started".to_string(),
        )
    })
}

/// The spellings of `true` and `false` a small model actually emits.
///
/// Recovery, not tolerance: anything NOT recognised answers `None`, and the
/// caller refuses with a sentence naming the field. Coercing an unrecognised
/// value to `false` would be the silent default this programme treats as a bug —
/// the caller asked for something and was told nothing.
fn coerce_bool(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "y" | "1" => Some(true),
            "false" | "no" | "n" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Remove the first present alias, if it holds a non-blank string.
fn take_alias(extra: &mut HashMap<String, serde_json::Value>, aliases: &[&str]) -> Option<String> {
    for alias in aliases {
        if let Some(value) = extra.get(*alias).and_then(|v| v.as_str()) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                extra.remove(*alias);
                return Some(value);
            }
        }
    }
    None
}

// ── Refusals ────────────────────────────────────────────────────────────────

/// Why a `delegate` call did not produce a child.
///
/// Split from the handler so every decision in this module is testable without
/// a live engine, a Goose session store or a database — the same split PAI-6 P2
/// made for `build_child_plan` and `classify_outcome`.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// No live turn holds an authority for this caller.
    ///
    /// Carries a trace label that is deliberately NOT part of
    /// [`message`](Self::message): the four inputs that land here must be
    /// indistinguishable to the model, and distinguishable in the log.
    Unauthorised { trace: &'static str },
    /// The caller is an unidentified speaker.
    Guest,
    /// The caller has already spent its one level of delegation.
    DepthExhausted,
    /// The payload did not parse as a [`TaskRequest`].
    Malformed(String),
    /// No recipe by that name.
    UnknownRole(String),
    /// A recipe exists but carries no `giap_role` block — an ordinary routine.
    NotARole(String),
    /// A recipe exists and its role block will not parse.
    UnreadableRole { role: String, message: String },
    /// The repository could not be read at all.
    RoleLookupFailed(String),
    /// The narrowing itself refused (depth, role mismatch, empty instructions).
    Narrowing(String),
    /// The orchestrator refused or failed to start the run.
    SpawnFailed(String),
    /// `check_task` was given an id this conversation has no task by.
    ///
    /// **Two inputs land here and they must be indistinguishable**, for the same
    /// reason [`Unauthorised`](Self::Unauthorised)'s four are: an id that no run
    /// ever had, and an id belonging to ANOTHER conversation's run. A caller
    /// that could tell those apart could enumerate the ids of delegations it was
    /// not party to and learn that they exist. The trace separates them; the
    /// message does not.
    UnknownTask {
        task_id: String,
        trace: &'static str,
    },
    /// The orchestrator could not be asked.
    TaskLookupFailed(String),
}

impl Refusal {
    /// The text the model reads.
    ///
    /// Every branch ends by telling the model what to do next, because a refusal
    /// a 2-4B model cannot act on is a refusal it will retry verbatim.
    pub fn message(&self) -> String {
        match self {
            // ONE string for all four unauthorised inputs. See the type's doc.
            Refusal::Unauthorised { .. } => "Delegation is not available in this conversation. \
                 Do the work yourself and tell the user what you found."
                .to_string(),
            Refusal::Guest => "Delegation is not available until this conversation is identified \
                 as a member of the household. Answer directly instead."
                .to_string(),
            Refusal::DepthExhausted => "You are already running as a delegated agent, and a \
                 delegated agent may not delegate again. Finish the task you were given."
                .to_string(),
            Refusal::Malformed(detail) => format!(
                "That is not a valid delegation: {detail}. Call delegate with `role` (the exact \
                 name of a saved role) and `instructions` (what it should do), and nothing else \
                 besides the optional `inputs` and `background`. You cannot choose the agent's \
                 permissions, tools or identity; they are derived from yours."
            ),
            Refusal::UnknownRole(role) => format!(
                "There is no saved role named `{role}` on this device, so there is nobody to \
                 delegate to. Do the work yourself."
            ),
            Refusal::NotARole(role) => format!(
                "`{role}` is a saved routine, not an agent role -- it has no `giap_role` block, \
                 so it cannot be delegated to. Do the work yourself."
            ),
            Refusal::UnreadableRole { role, message } => format!(
                "The saved role `{role}` cannot be read: {message}. Do not guess at what it was \
                 meant to say -- tell the user the role needs fixing, and do the work yourself."
            ),
            Refusal::RoleLookupFailed(detail) => format!(
                "The saved roles could not be read ({detail}), so this delegation cannot be \
                 checked. Do the work yourself."
            ),
            Refusal::Narrowing(detail) => format!("Delegation refused: {detail}."),
            Refusal::SpawnFailed(detail) => format!(
                "The delegated agent could not be started: {detail}. Do the work yourself and \
                 tell the user."
            ),
            // ONE string for both unknown-task inputs. See the variant's doc.
            Refusal::UnknownTask { .. } => "There is no delegated task by that id in this \
                 conversation. If you started one, use the id you were given; otherwise there is \
                 nothing to check."
                .to_string(),
            Refusal::TaskLookupFailed(detail) => format!(
                "The delegated task could not be checked ({detail}). Tell the user, and do not \
                 guess at what it found."
            ),
        }
    }

    /// A stable label for the log line. Never shown to the model.
    pub fn trace(&self) -> &'static str {
        match self {
            Refusal::Unauthorised { trace } => trace,
            Refusal::Guest => "guest",
            Refusal::DepthExhausted => "depth_exhausted",
            Refusal::Malformed(_) => "malformed_request",
            Refusal::UnknownRole(_) => "unknown_role",
            Refusal::NotARole(_) => "not_a_role",
            Refusal::UnreadableRole { .. } => "unreadable_role",
            Refusal::RoleLookupFailed(_) => "role_lookup_failed",
            Refusal::Narrowing(_) => "narrowing_refused",
            Refusal::SpawnFailed(_) => "spawn_failed",
            Refusal::UnknownTask { trace, .. } => trace,
            Refusal::TaskLookupFailed(_) => "task_lookup_failed",
        }
    }
}

// ── The decision, as pure functions ─────────────────────────────────────────

/// May this caller delegate at all?
///
/// Called BEFORE the recipe is looked up, and that ordering is the point rather
/// than an optimisation: an unauthorised caller must not be able to make this
/// pond read its database, and a caller that is refused should be told the
/// reason it was actually refused for rather than "no such role".
///
/// Three refusals, three mechanisms, none of them the same as another's:
///
/// - **`None`.** The lease behind the entry is gone, or there never was one.
/// - **Guest.** Second enforcement. The first is
///   `groups_denied_to_guests`, which keeps `giap-orchestrator__delegate` out of
///   an unidentified speaker's allow-set in both selection modes. That layer is
///   the one that works today; this one is what still refuses if a future change
///   moves, reorders or bypasses it — which is exactly what happened to PAI-1 P5.
/// - **Depth.** [`DelegationAuthority::may_delegate`] answers it. Note honestly
///   that deleting this line does not open a hole: `delegate` refuses again with
///   `DepthExceeded`. What it changes is that the refusal arrives after a
///   database read and reads as "no such role" when the role is missing too.
pub fn authorise(authority: Option<DelegationAuthority>) -> Result<DelegationAuthority, Refusal> {
    let Some(authority) = authority else {
        return Err(Refusal::Unauthorised {
            trace: "no_live_turn",
        });
    };
    if authority.profile_scope().excludes_everything() {
        return Err(Refusal::Guest);
    }
    if !authority.may_delegate() {
        return Err(Refusal::DepthExhausted);
    }
    Ok(authority)
}

/// What reading the named recipe produced.
///
/// Four outcomes rather than an `Option`, because the two failure directions are
/// not the same refusal and must not collapse into one:
/// [`Unreadable`](Self::Unreadable) is a human's mistake in a role block, and
/// answering it with a default role is the widening path.
#[derive(Debug, Clone, PartialEq)]
pub enum RoleLookup {
    /// No recipe by that name.
    Missing,
    /// A recipe with no `giap_role` block: an ordinary routine.
    NotARole,
    /// A recipe whose `giap_role` block will not parse or will not validate.
    Unreadable(RoleError),
    Found(AgentRole),
}

impl RoleLookup {
    /// Read a role out of a recipe's YAML, or say why not.
    pub fn from_recipe(name: &str, yaml: Option<&str>) -> Self {
        let Some(yaml) = yaml else {
            return RoleLookup::Missing;
        };
        match AgentRole::from_recipe_yaml(name, yaml) {
            Ok(Some(role)) => RoleLookup::Found(role),
            Ok(None) => RoleLookup::NotARole,
            Err(e) => RoleLookup::Unreadable(e),
        }
    }
}

/// Turn an authorised caller, a role lookup and a request into a [`TaskSpec`].
///
/// Every narrowing decision belongs to
/// [`DelegationAuthority::delegate`] — this function's whole job is to refuse
/// the four ways the role can fail to exist and then get out of the way.
pub fn decide(
    authority: &DelegationAuthority,
    lookup: RoleLookup,
    request: TaskRequest,
) -> Result<TaskSpec, Refusal> {
    let role = match lookup {
        RoleLookup::Missing => return Err(Refusal::UnknownRole(request.role)),
        RoleLookup::NotARole => return Err(Refusal::NotARole(request.role)),
        RoleLookup::Unreadable(e) => {
            return Err(Refusal::UnreadableRole {
                role: request.role,
                message: e.to_string(),
            })
        }
        RoleLookup::Found(role) => role,
    };
    authority
        .delegate(&role, request)
        .map_err(|e| Refusal::Narrowing(e.to_string()))
}

/// Whose task is this, and may this caller see it? — PAI-6 P8.
///
/// **The scoping is the substance and it is not incidental.**
/// `Orchestrator::poll` is keyed by task id alone, and a task id is a v4 UUID
/// the model was told — so without this, a `check_task` call could read back the
/// result of a delegation started in ANOTHER conversation, which on this pond
/// means another household member's. `TaskRun.parent_session_id` is the GIAP
/// session, and the caller's is the one PAI-6 P3 resolved from the engine's own
/// `_meta`; a match is the only thing that permits an answer.
///
/// A foreign task and a nonexistent one produce the same refusal, deliberately:
/// see [`Refusal::UnknownTask`].
pub fn authorise_task(
    run: Option<TaskRun>,
    caller_session_id: &str,
    task_id: &str,
) -> Result<TaskRun, Refusal> {
    let Some(run) = run else {
        return Err(Refusal::UnknownTask {
            task_id: task_id.to_string(),
            trace: "no_such_task",
        });
    };
    if run.parent_session_id != caller_session_id {
        return Err(Refusal::UnknownTask {
            task_id: task_id.to_string(),
            trace: "task_of_another_session",
        });
    }
    Ok(run)
}

/// What the parent is told about a finished run.
///
/// Reads the answer through [`TaskRun::result_for_parent`] and never through
/// `result` directly. That is not defensive style: Goose's child loop returns
/// `Ok(partial_text)` when a child is cancelled and returns the literal
/// max-turns sentence as its "answer" when the budget runs out, so a run that
/// was stopped can carry text that reads exactly like a result. Handing it to
/// the parent would report an abandoned run as a finding.
pub fn describe_run(run: &TaskRun) -> String {
    if let Some(answer) = run.result_for_parent() {
        return format!("The `{}` agent reports:\n\n{answer}", run.role);
    }
    match run.status {
        TaskStatus::Completed => format!(
            "The `{}` agent finished without producing anything to report.",
            run.role
        ),
        TaskStatus::Cancelled => format!(
            "The `{}` agent was stopped before it finished. Nothing it produced can be relied on.",
            run.role
        ),
        TaskStatus::TurnBudgetExhausted => format!(
            "The `{}` agent ran out of the actions it was allowed and did not reach an answer. \
             Do not treat this as a result -- either do the work yourself or ask the user a \
             narrower question.",
            run.role
        ),
        TaskStatus::Failed => format!(
            "The `{}` agent failed: {}.",
            run.role,
            run.error.as_deref().unwrap_or("no reason given")
        ),
        TaskStatus::Queued | TaskStatus::Running => format!(
            "The `{}` agent is still working (task {}). Tell the user it is running.",
            run.role, run.id
        ),
    }
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct OrchestratorMcpServer {
    /// `None` in tests and in any entry point that never built an agent. Every
    /// call then refuses as unauthorised rather than proceeding — the same
    /// direction `DraftMcpServer` takes for a missing authority, and the
    /// opposite of the direction that made the direct dispatcher a bypass.
    deps: Option<OrchestratorDeps>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl OrchestratorMcpServer {
    pub fn new(deps: Option<OrchestratorDeps>) -> Self {
        Self {
            deps,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Hand a piece of work to a saved specialist agent and wait for what it finds. Give it `role` \
(the exact name of a saved role on this device) and `instructions` (what it should do). The \
agent runs with a NARROWER set of tools and permissions than you have -- derived from yours, \
never chosen -- so you cannot ask for its scope, tools or identity. Use it for work that would \
otherwise take you many steps; do simple things yourself. Set `background` to true only for \
long work you do not need the answer to right now: you get a task id back instead of an answer \
and check it later with check_task, and on a pond that runs its model on the device itself this \
is refused, because there one agent can work at a time.")]
    async fn delegate(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<DelegateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("delegate");
        let text = match self.run_delegation(&ctx.meta, params.0).await {
            Ok(text) => text,
            Err(refusal) => {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "delegation_refused",
                    reason = refusal.trace(),
                    "a delegate call was refused"
                );
                refusal.message()
            }
        };
        // A refusal comes back as tool SUCCESS carrying the explanation, the
        // same as `giap-toolkit` and `giap-draft`: an MCP protocol error makes
        // small models retry the identical bad call, whereas a sentence naming
        // what to do instead lets them self-correct.
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// The whole decision, with the two I/O steps in the middle.
    async fn run_delegation(
        &self,
        meta: &rmcp::model::Meta,
        params: DelegateParams,
    ) -> Result<String, Refusal> {
        let Some(deps) = &self.deps else {
            return Err(Refusal::Unauthorised {
                trace: "deps_not_installed",
            });
        };
        // A missing `_meta` is the fourth unauthorised input, and it gets the
        // same message as the other three by construction: there is one
        // `Unauthorised` variant and one string on it.
        let Some(session) = crate::session_from_meta(meta) else {
            return Err(Refusal::Unauthorised {
                trace: "no_engine_session",
            });
        };
        let authority = authorise(deps.authorities.authority_for_engine_session(&session))?;

        // Only now is the payload parsed and the database touched.
        let request = into_request(params)?;
        let recipe = deps
            .recipes
            .get_by_name(&request.role)
            .await
            .map_err(|e| Refusal::RoleLookupFailed(e.to_string()))?;
        let lookup =
            RoleLookup::from_recipe(&request.role, recipe.as_ref().map(|r| r.yaml.as_str()));

        let spec = decide(&authority, lookup, request)?;
        tracing::info!(
            target: "giap::trace",
            kind = "delegation_authorised",
            task_id = %spec.id(),
            role = %spec.role(),
            parent_session_id = %spec.parent_session_id(),
            groups = ?spec.tool_groups(),
            depth = spec.depth().get(),
            max_turns = spec.max_turns(),
            background = spec.background(),
        );
        let run = deps
            .orchestrator
            .spawn(spec)
            .await
            .map_err(|e| Refusal::SpawnFailed(e.to_string()))?;
        Ok(describe_run(&run))
    }

    #[tool(description = "\
Check on a delegated agent you started earlier with delegate and `background`. Give it the \
`task_id` you were told. It answers with what that agent is doing, or with what it found if it \
has finished. You can only check tasks started in this conversation.")]
    async fn check_task(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<CheckTaskParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("check_task");
        let text = match self.run_check(&ctx.meta, params.0).await {
            Ok(text) => text,
            Err(refusal) => {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "check_task_refused",
                    reason = refusal.trace(),
                    "a check_task call was refused"
                );
                refusal.message()
            }
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// The whole of `check_task`, with the one I/O step in the middle.
    ///
    /// It authorises the CALLER exactly as `delegate` does before it looks
    /// anything up — a guest, a subagent and a caller with no live turn are
    /// refused here too, and for the same reasons — and then authorises the
    /// TASK, which is the part that is this tool's own.
    async fn run_check(
        &self,
        meta: &rmcp::model::Meta,
        params: CheckTaskParams,
    ) -> Result<String, Refusal> {
        let Some(deps) = &self.deps else {
            return Err(Refusal::Unauthorised {
                trace: "deps_not_installed",
            });
        };
        let Some(session) = crate::session_from_meta(meta) else {
            return Err(Refusal::Unauthorised {
                trace: "no_engine_session",
            });
        };
        let authority = authorise(deps.authorities.authority_for_engine_session(&session))?;

        let task_id = check_task_id(params)?;
        let run = deps
            .orchestrator
            .poll(&task_id)
            .await
            .map_err(|e| Refusal::TaskLookupFailed(e.to_string()))?;
        let run = authorise_task(run, authority.session_id(), &task_id)?;
        Ok(describe_run(&run))
    }
}

#[tool_handler]
impl ServerHandler for OrchestratorMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                ORCHESTRATOR_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Orchestrator MCP server — hand work to a saved specialist agent.\n\n\
                 A delegated agent runs on this device with a narrower set of tools than you \
                 have and no ability to delegate further. You choose only which saved role to \
                 run and what to ask it; everything else is derived. If there is no suitable \
                 saved role, do the work yourself rather than inventing one.",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

/// Everything the `delegate` tool needs, none of which exists at registration
/// time.
#[derive(Clone)]
pub struct OrchestratorDeps {
    orchestrator: Arc<dyn Orchestrator>,
    authorities: Arc<TurnAuthorityRegistry>,
    recipes: Arc<dyn AgentRecipeRepository + Send + Sync>,
}

impl OrchestratorDeps {
    pub fn new(
        orchestrator: Arc<dyn Orchestrator>,
        authorities: Arc<TurnAuthorityRegistry>,
        recipes: Arc<dyn AgentRecipeRepository + Send + Sync>,
    ) -> Self {
        Self {
            orchestrator,
            authorities,
            recipes,
        }
    }
}

static ORCHESTRATOR_DEPS: OnceLock<OrchestratorDeps> = OnceLock::new();

/// Install the orchestrator, the turn-authority registry and the recipe store.
///
/// Call once at startup, AFTER the agent adapter exists — it owns both the
/// child runner and the registry, and neither exists when
/// `register_giap_extensions` runs. **The registry must be the SAME one the
/// adapter publishes into** (`GooseAdapter::turn_authorities()`); a second
/// registry would answer `None` to every lookup, and every delegation would be
/// refused as unauthorised.
///
/// Absent, the tool refuses every call. That is the correct direction and the
/// one the direct dispatcher got wrong for `giap-draft`: a missing authority
/// must refuse, never proceed.
pub fn init_orchestrator_deps(deps: OrchestratorDeps) {
    let _ = ORCHESTRATOR_DEPS.set(deps);
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_orchestrator_server(reader: DuplexStream, writer: DuplexStream) {
    // Like `giap-toolkit` and unlike the rest, this does NOT expect(): the
    // extension is registered before the adapter that supplies its deps is
    // built, so a missing handle is a legitimate transient state. Unlike
    // `giap-toolkit`, the degraded behaviour is refusal rather than a truthful
    // "nothing to do".
    let server = OrchestratorMcpServer::new(ORCHESTRATOR_DEPS.get().cloned());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-orchestrator MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::mcp::domain::tool_group::{groups_denied_to_subagents, ORCHESTRATOR_EXTENSION};
    use pond_core::user_data::domain::profile::ProfileScope;
    use std::collections::BTreeSet;

    fn params(json: serde_json::Value) -> DelegateParams {
        serde_json::from_value(json).expect("DelegateParams accepts any object")
    }

    fn groups(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn root(scope: ProfileScope) -> DelegationAuthority {
        DelegationAuthority::root("giap-session", scope, groups(&["giap-weather"]))
    }

    fn researcher_yaml() -> &'static str {
        "title: Researcher\n\
         giap_role:\n  \
           tool_groups: [giap-weather]\n  \
           instructions: Look it up and stop.\n  \
           max_turns: 3\n"
    }

    // ── authorise ──────────────────────────────────────────────────────────

    /// PAI-6 P1's contract: a session that never chatted, a subagent's own
    /// session and a turn that has ended all answer `None`, and **all three must
    /// refuse identically**. The fourth input — a call with no `_meta` at all —
    /// is folded into the same variant for the same reason.
    ///
    /// Asserted on the exact strings rather than on the variant, because the
    /// variant being shared is an implementation detail and the property is
    /// about what the model can distinguish.
    #[test]
    fn every_unauthorised_input_is_refused_with_the_same_words() {
        let from_registry = authorise(None).unwrap_err();
        let no_meta = Refusal::Unauthorised {
            trace: "no_engine_session",
        };
        let no_deps = Refusal::Unauthorised {
            trace: "deps_not_installed",
        };
        assert_eq!(from_registry.message(), no_meta.message());
        assert_eq!(from_registry.message(), no_deps.message());
        // ... and the log can still tell them apart, which is the whole reason
        // the trace is a separate accessor.
        assert_ne!(from_registry.trace(), no_meta.trace());
        assert_ne!(no_meta.trace(), no_deps.trace());
    }

    #[test]
    fn a_guest_turn_is_refused_even_though_its_authority_is_a_root_one() {
        let guest = root(ProfileScope::Guest);
        // Vacuity control: the guest's authority is depth 0 and non-empty, so
        // the refusal below is about the SCOPE and not about there being
        // nothing to delegate with.
        assert!(guest.may_delegate());
        assert!(!guest.tool_groups().is_empty());
        assert_eq!(authorise(Some(guest)).unwrap_err(), Refusal::Guest);
    }

    #[test]
    fn an_identified_turn_is_authorised() {
        for scope in [ProfileScope::Household, ProfileScope::Owner("jerry".into())] {
            assert!(
                authorise(Some(root(scope.clone()))).is_ok(),
                "{scope:?} must be able to delegate, or the feature does not exist"
            );
        }
    }

    #[test]
    fn a_subagents_authority_is_refused_on_depth() {
        let parent = root(ProfileScope::Household);
        let role = AgentRole::new(
            "researcher",
            "go",
            groups(&["giap-weather"]),
            Default::default(),
            3,
            0.5,
        )
        .unwrap();
        let spec = parent
            .delegate(
                &role,
                TaskRequest {
                    role: "researcher".into(),
                    instructions: "look it up".into(),
                    inputs: serde_json::Value::Null,
                    background: false,
                },
            )
            .unwrap();
        let child = spec.child_authority("child-session");
        assert_eq!(
            authorise(Some(child)).unwrap_err(),
            Refusal::DepthExhausted,
            "a delegated agent must not be able to delegate again"
        );
    }

    /// The other half of "do not offer a tool whose every call is refused":
    /// the depth check above is the enforcement, and this is what stops a child
    /// ever seeing the tool. Both, because neither implies the other.
    #[test]
    fn the_delegation_group_is_withheld_from_subagents_as_well_as_refused() {
        assert!(
            groups_denied_to_subagents().contains(&ORCHESTRATOR_EXTENSION),
            "a role naming giap-orchestrator under a parent that holds it would put a tool in a \
             child's prompt whose every call authorise() refuses"
        );
    }

    // ── into_request ───────────────────────────────────────────────────────

    #[test]
    fn a_well_formed_call_parses() {
        let request = into_request(params(serde_json::json!({
            "role": "researcher",
            "instructions": "find out when the bins go out"
        })))
        .expect("a role and instructions is all a caller supplies");
        assert_eq!(request.role, "researcher");
        assert_eq!(request.instructions, "find out when the bins go out");
        assert_eq!(request.inputs, serde_json::Value::Null);
    }

    #[test]
    fn the_aliases_a_small_model_invents_are_recovered() {
        for role_alias in ROLE_ALIASES {
            for instruction_alias in INSTRUCTION_ALIASES {
                let request = into_request(params(serde_json::json!({
                    role_alias: "researcher",
                    instruction_alias: "go"
                })))
                .unwrap_or_else(|e| {
                    panic!("{role_alias}/{instruction_alias} was not recovered: {e:?}")
                });
                assert_eq!(request.role, "researcher");
                assert_eq!(request.instructions, "go");
            }
        }
    }

    /// **The guard on the alias list.** `TaskRequest` is `deny_unknown_fields`
    /// so that a model cannot name its own scope, tools, depth or session; that
    /// property survives only while no alias is a synonym for one of them.
    ///
    /// Quantified over the field names the domain deliberately does not have,
    /// under both a bare spelling and the spelling of a real field, so that
    /// adding `"session_id"` to either alias list fails here rather than in
    /// production.
    #[test]
    fn a_widening_field_is_refused_under_every_spelling() {
        for widening in [
            "profile_scope",
            "scope",
            "tool_groups",
            "tools",
            "depth",
            "session_id",
            "parent_session_id",
            "max_turns",
            "context_fraction",
        ] {
            let refusal = into_request(params(serde_json::json!({
                "role": "researcher",
                "instructions": "go",
                widening: "anything at all"
            })))
            .unwrap_err();
            assert!(
                matches!(refusal, Refusal::Malformed(_)),
                "a delegate call carrying `{widening}` was accepted; the caller must not be able \
                 to name any part of the child's authority"
            );
            assert!(
                refusal.message().contains(widening),
                "the refusal for `{widening}` does not name the offending field, so a model \
                 cannot correct it: {}",
                refusal.message()
            );
        }
    }

    #[test]
    fn a_call_with_no_instructions_is_refused_rather_than_defaulted() {
        let refusal =
            into_request(params(serde_json::json!({ "role": "researcher" }))).unwrap_err();
        assert!(matches!(refusal, Refusal::Malformed(_)));
        assert!(refusal.message().contains("instructions"));
    }

    // ── RoleLookup / decide ────────────────────────────────────────────────

    fn request() -> TaskRequest {
        TaskRequest {
            role: "researcher".into(),
            instructions: "look it up".into(),
            inputs: serde_json::Value::Null,
            background: false,
        }
    }

    #[test]
    fn a_recipe_carrying_a_role_produces_a_narrowed_spec() {
        let authority = root(ProfileScope::Household);
        let lookup = RoleLookup::from_recipe("researcher", Some(researcher_yaml()));
        assert!(matches!(lookup, RoleLookup::Found(_)));
        let spec = decide(&authority, lookup, request()).expect("a valid role delegates");
        assert_eq!(spec.role(), "researcher");
        assert_eq!(spec.depth().get(), 1);
        assert!(spec.grants_tool("giap-weather__get_forecast"));
        // The narrowing really happened: the child did not inherit the parent's
        // ability to delegate, and holds nothing the denylist names.
        assert!(!spec.child_authority("c").may_delegate());
        for denied in groups_denied_to_subagents() {
            assert!(!spec.tool_groups().contains(*denied));
        }
    }

    #[test]
    fn a_recipe_that_is_not_a_role_is_refused_rather_than_run() {
        let authority = root(ProfileScope::Household);
        let lookup = RoleLookup::from_recipe("morning_brief", Some("title: Morning Brief\n"));
        assert_eq!(lookup, RoleLookup::NotARole);
        assert!(matches!(
            decide(&authority, lookup, request()),
            Err(Refusal::NotARole(_))
        ));
    }

    #[test]
    fn a_missing_recipe_is_refused() {
        let authority = root(ProfileScope::Household);
        assert_eq!(RoleLookup::from_recipe("nope", None), RoleLookup::Missing);
        assert!(matches!(
            decide(&authority, RoleLookup::Missing, request()),
            Err(Refusal::UnknownRole(_))
        ));
    }

    /// **The direction that matters.** A `giap_role` block that will not parse
    /// or will not validate must refuse, never fall back to a default role: the
    /// fallback would hand a child whatever the default tool set is, on exactly
    /// the input a human got wrong.
    #[test]
    fn an_unreadable_role_refuses_instead_of_substituting_a_default() {
        let authority = root(ProfileScope::Household);
        for (label, yaml) in [
            // `deny_unknown_fields` inside the block: a typo on the one required
            // field is a parse error, not an absent narrowing.
            (
                "misspelled tool_groups",
                "giap_role:\n  toolgroups: [giap-weather]\n  instructions: go\n",
            ),
            // Present, parses, fails validation.
            (
                "turn budget past the cap",
                "giap_role:\n  tool_groups: [giap-weather]\n  instructions: go\n  max_turns: 99\n",
            ),
            (
                "no instructions anywhere",
                "giap_role:\n  tool_groups: [giap-weather]\n",
            ),
        ] {
            let lookup = RoleLookup::from_recipe("researcher", Some(yaml));
            assert!(
                matches!(lookup, RoleLookup::Unreadable(_)),
                "{label}: expected an unreadable role, got {lookup:?}"
            );
            let refusal = decide(&authority, lookup, request()).unwrap_err();
            assert!(
                matches!(refusal, Refusal::UnreadableRole { .. }),
                "{label}: {refusal:?}"
            );
        }
    }

    /// Vacuity control for the test above: the same reader accepts the valid
    /// role, so "unreadable" is a property of those documents rather than of a
    /// parser that rejects everything.
    #[test]
    fn the_same_reader_accepts_a_valid_role() {
        assert!(matches!(
            RoleLookup::from_recipe("researcher", Some(researcher_yaml())),
            RoleLookup::Found(_)
        ));
    }

    // ── describe_run ───────────────────────────────────────────────────────

    fn run_with(status: TaskStatus, result: Option<&str>) -> TaskRun {
        TaskRun {
            id: "task-1".into(),
            role: "researcher".into(),
            parent_session_id: "giap-session".into(),
            status,
            result: result.map(str::to_string),
            error: None,
            started_at: chrono::Utc::now(),
            finished_at: None,
        }
    }

    /// Goose returns `Ok(partial_text)` on cancellation and the literal max-turns
    /// sentence on budget exhaustion, so a stopped run carries text that reads
    /// like an answer. It must not reach the parent.
    #[test]
    fn only_a_completed_run_hands_its_text_to_the_parent() {
        const SECRET: &str = "the bins go out on Thursday";
        for status in [
            TaskStatus::Cancelled,
            TaskStatus::TurnBudgetExhausted,
            TaskStatus::Failed,
            TaskStatus::Queued,
            TaskStatus::Running,
        ] {
            let described = describe_run(&run_with(status, Some(SECRET)));
            assert!(
                !described.contains(SECRET),
                "{status:?} leaked the child's partial text to the parent: {described}"
            );
        }
        // Vacuity control: the text DOES come through when the run completed,
        // so the assertions above are about the status rather than about
        // describe_run never quoting anything.
        let completed = describe_run(&run_with(TaskStatus::Completed, Some(SECRET)));
        assert!(completed.contains(SECRET));
    }

    // ── background + check_task (PAI-6 P8) ─────────────────────────────────

    #[test]
    fn a_delegation_is_synchronous_unless_the_caller_asks_otherwise() {
        let request = into_request(params(serde_json::json!({
            "role": "researcher",
            "instructions": "go"
        })))
        .unwrap();
        assert!(
            !request.background,
            "a caller that said nothing about background got a run that outlives its turn"
        );
    }

    /// The spellings a 2-4B model actually emits. Declaring the field
    /// `Option<bool>` would make every one of these an rmcp deserialization
    /// failure of the whole call — a protocol error, which is the thing this
    /// module exists to avoid.
    #[test]
    fn the_boolean_spellings_a_small_model_emits_are_recovered() {
        for (spelling, expected) in [
            (serde_json::json!(true), true),
            (serde_json::json!("true"), true),
            (serde_json::json!("True"), true),
            (serde_json::json!("yes"), true),
            (serde_json::json!("1"), true),
            (serde_json::json!(false), false),
            (serde_json::json!("false"), false),
            (serde_json::json!("no"), false),
        ] {
            let request = into_request(params(serde_json::json!({
                "role": "researcher",
                "instructions": "go",
                "background": spelling
            })))
            .unwrap_or_else(|e| panic!("`background: {spelling}` was not recovered: {e:?}"));
            assert_eq!(request.background, expected, "background: {spelling}");
        }
    }

    /// The other direction, and the one that matters: an unrecognised value is
    /// REFUSED with a sentence naming the field, never coerced to `false`. A
    /// caller that asked for something and was silently given the opposite has
    /// no way to find out.
    #[test]
    fn an_unreadable_background_value_refuses_rather_than_defaulting() {
        for nonsense in [
            serde_json::json!("later"),
            serde_json::json!(7),
            serde_json::json!({"when": "later"}),
        ] {
            let refusal = into_request(params(serde_json::json!({
                "role": "researcher",
                "instructions": "go",
                "background": nonsense
            })))
            .unwrap_err();
            assert!(
                matches!(refusal, Refusal::Malformed(_)),
                "`background: {nonsense}` was accepted"
            );
            assert!(
                refusal.message().contains("background"),
                "the refusal does not name the field, so the model cannot correct it: {}",
                refusal.message()
            );
        }
    }

    fn run_of(session: &str, id: &str) -> TaskRun {
        TaskRun {
            id: id.into(),
            role: "researcher".into(),
            parent_session_id: session.into(),
            status: TaskStatus::Completed,
            result: Some("the bins go out on Thursday".into()),
            error: None,
            started_at: chrono::Utc::now(),
            finished_at: None,
        }
    }

    /// **The scoping, which is what stops `check_task` being a read of anyone
    /// else's delegation.** `Orchestrator::poll` is keyed by task id alone, and
    /// an id is a UUID the model was told — so on a household pond, without
    /// this, one member's conversation could read back another's result.
    #[test]
    fn a_task_belonging_to_another_conversation_is_not_readable() {
        const SECRET: &str = "the bins go out on Thursday";
        let theirs = run_of("someone-elses-session", "task-1");
        let refusal = authorise_task(Some(theirs), "my-session", "task-1").unwrap_err();
        assert!(
            matches!(refusal, Refusal::UnknownTask { .. }),
            "{refusal:?}"
        );
        assert!(
            !refusal.message().contains(SECRET),
            "the refusal leaked the other conversation's result: {}",
            refusal.message()
        );
    }

    /// Vacuity control: the caller's OWN task is readable, so the test above is
    /// about the session and not about `authorise_task` refusing everything.
    #[test]
    fn a_task_of_this_conversation_is_readable() {
        let mine = run_of("my-session", "task-1");
        let run = authorise_task(Some(mine), "my-session", "task-1").expect("my own task");
        assert_eq!(run.id, "task-1");
        assert!(describe_run(&run).contains("the bins go out on Thursday"));
    }

    /// The same words for both, so a caller cannot probe which task ids exist —
    /// the same property `every_unauthorised_input_is_refused_with_the_same_words`
    /// asserts for the four unauthorised inputs, and for the same reason.
    #[test]
    fn a_missing_task_and_somebody_elses_are_indistinguishable_to_the_caller() {
        let missing = authorise_task(None, "my-session", "task-1").unwrap_err();
        let theirs =
            authorise_task(Some(run_of("other", "task-1")), "my-session", "task-1").unwrap_err();
        assert_eq!(missing.message(), theirs.message());
        assert_ne!(
            missing.trace(),
            theirs.trace(),
            "the log cannot tell a hallucinated id from a probe of another conversation"
        );
    }

    #[test]
    fn a_check_with_no_id_is_refused_rather_than_guessed_at() {
        let refusal = check_task_id(CheckTaskParams::default()).unwrap_err();
        assert!(matches!(refusal, Refusal::Malformed(_)));
        assert!(refusal.message().contains("task_id"));
    }

    #[test]
    fn the_task_id_aliases_a_small_model_invents_are_recovered() {
        for alias in TASK_ID_ALIASES {
            let parsed: CheckTaskParams =
                serde_json::from_value(serde_json::json!({ alias: "task-1" }))
                    .expect("CheckTaskParams accepts any object");
            assert_eq!(
                check_task_id(parsed).unwrap_or_else(|e| panic!("{alias}: {e:?}")),
                "task-1"
            );
        }
    }

    /// A background run comes back non-terminal, and `describe_run` must tell
    /// the model it is running and hand it no answer — the same path
    /// `only_a_completed_run_hands_its_text_to_the_parent` covers, said for the
    /// state P8 made reachable.
    #[test]
    fn a_running_background_task_is_reported_with_its_id_and_no_answer() {
        let mut run = run_of("my-session", "task-7");
        run.status = TaskStatus::Queued;
        let described = describe_run(&run);
        assert!(described.contains("task-7"), "{described}");
        assert!(described.contains("still working"), "{described}");
        assert!(
            !described.contains("the bins go out on Thursday"),
            "a queued run handed the parent text it has not earned: {described}"
        );
    }

    #[test]
    fn an_exhausted_budget_is_not_reported_as_an_answer() {
        let described = describe_run(&run_with(TaskStatus::TurnBudgetExhausted, None));
        assert!(described.contains("ran out"));
        assert!(described.contains("Do not treat this as a result"));
    }
}
