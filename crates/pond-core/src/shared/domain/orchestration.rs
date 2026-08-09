//! Multi-agent orchestration domain — PAI-6 P1.
//!
//! What may run as a subagent, under whose authority, and with what limits.
//! **Policy only.** How a child agent is actually executed belongs to the
//! adapter (PAI-6 P2); nothing here knows that Goose exists, and nothing here
//! spawns anything.
//!
//! # The two invariants this module tries to make the compiler enforce
//!
//! PAI-6's invariant 1 is "a subagent's scope is a subset of its parent's,
//! never wider — not tools, not profile, not network", and invariant 6 is
//! "depth is capped". Both are usually written as runtime checks that a later
//! refactor deletes. Here they are structural:
//!
//! - **A caller cannot name a scope for a child.** [`TaskRequest`] — the part a
//!   model supplies through the `delegate` tool — has no scope field, no tool
//!   field and no depth field, and it is `deny_unknown_fields`, so a payload
//!   that tries to carry one is refused rather than silently ignored. The
//!   child's scope is computed from the parent's by [`RolePersonalData`], an
//!   enum with **no variant that widens**: it either inherits the parent's
//!   scope unchanged or drops to [`ProfileScope::Guest`]. There is no value of
//!   any type in this module that expresses "run wider than my parent".
//! - **A child cannot forge a depth.** [`DelegationDepth`] wraps a private
//!   `u8`, derives no `Deserialize` and no `Default`, and its only increment is
//!   a private method. The one public path from an authority to a deeper
//!   authority is [`DelegationAuthority::delegate`] → [`TaskSpec`] →
//!   [`TaskSpec::child_authority`], and it refuses at
//!   [`MAX_DELEGATION_DEPTH`]. A subagent is handed its authority; it never
//!   builds one.
//!
//! # Invariant 1's third axis, and the accident that satisfies it
//!
//! The invariant reads "not tools, not profile, not **network**", and this
//! module has nothing to say about network. That axis holds today by accident
//! of another workstream: PAI-2's network mode is a process-global
//! `static RwLock<NetworkMode>` in [`crate::shared::services::egress`], so an
//! in-process child physically cannot hold a mode its parent does not. The
//! accident expires the day a child runs out of process, or the day network
//! mode becomes per-session — at which point the axis needs a field on
//! [`TaskSpec`] like the other two. Nothing will notice on its own: there is no
//! test here to fail, because there is not yet anything to test.
//!
//! # What is deliberately NOT here
//!
//! No persistence. A role is stored inside the YAML of an existing row in the
//! `agent_recipes` table (see [`AgentRole::from_recipe_yaml`]), under one
//! GIAP-owned key that Goose and `pond-api`'s recipe runner both ignore.
//! PAI-6 P1 adds no migration and no table.

use crate::mcp::domain::tool_group::{
    group_of_tool, groups_denied_to_guests, groups_denied_to_subagents,
};
use crate::models::services::context::model_class::runs_on_this_device;
use crate::user_data::domain::profile::ProfileScope;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

/// How many delegations deep a turn may go. A root turn is depth 0, so a cap of
/// 1 means "the user's turn may delegate; a subagent may not".
///
/// PAI-6 invariant 6: "a recursive delegation loop on a home server is a fire."
/// Raising this is a decision, not a tuning knob — every level multiplies the
/// number of concurrent claims on one GPU and one context window.
pub const MAX_DELEGATION_DEPTH: u8 = 1;

/// Turn budget for a role that does not state one.
///
/// Goose's own default is 25 (`DEFAULT_SUBAGENT_MAX_TURNS`), which is a
/// coding-agent number: twenty-five turns of a 2-4B model on an Orin is minutes
/// of wall clock during which the parent turn is blocked. Six is enough for
/// "look three things up and summarise" and short enough that a looping
/// subagent is noticed rather than endured.
pub const DEFAULT_ROLE_MAX_TURNS: u32 = 6;

/// The largest turn budget a stored role may ask for.
///
/// A refusal rather than a silent clamp: a role asking for 40 turns was written
/// by somebody who expected 40, and quietly giving them 12 is the kind of
/// disagreement that gets diagnosed as a model failure.
pub const MAX_ROLE_MAX_TURNS: u32 = 12;

/// Share of the parent's history budget a role claims when it does not say.
pub const DEFAULT_CONTEXT_FRACTION: f32 = 0.5;

/// The one key GIAP owns inside a recipe's YAML.
///
/// Namespaced so it cannot collide with a Goose recipe field. Goose ignores it
/// (its `Recipe` is not `deny_unknown_fields`), and so does `pond-api`'s
/// `RecipePrompt`, so a recipe carrying a role still runs as an ordinary
/// routine.
pub const ROLE_YAML_KEY: &str = "giap_role";

/// How many subagents may run at once against `provider`.
///
/// **Read the predicate, not the invariant text.** PAI-6 invariant 3 says
/// "concurrency is 1 on `local`/`gguf`", and there is a predicate in this tree
/// that means exactly that — `context_governor::is_local_provider`. Using it
/// here would be a bug this programme has already made once and fixed once
/// (PAI-4 P2): `ollama` and `llamafile` speak HTTP, but on a GIAP pond they
/// speak it to `127.0.0.1`, so they are the same GPU the parent's next turn
/// needs. [`runs_on_this_device`] is the wider deny set and therefore the safe
/// one.
///
/// The remote number is a guess and is labelled as one — nothing has measured
/// subagent concurrency against a hosted provider from this codebase.
pub fn max_concurrent_subagents(provider: &str) -> usize {
    if runs_on_this_device(provider) {
        1
    } else {
        REMOTE_SUBAGENT_CONCURRENCY
    }
}

/// Concurrent subagents permitted against a provider that runs somewhere else.
/// Unmeasured; see [`max_concurrent_subagents`].
pub const REMOTE_SUBAGENT_CONCURRENCY: usize = 3;

// ── Depth ───────────────────────────────────────────────────────────────────

/// How many delegations deep a turn already is.
///
/// Deliberately **not** `Deserialize` and **not** `Default`. A `#[serde(default)]`
/// `u8` deserializes to 0, which reads as "I am the root, I may spawn" — the
/// widening direction on every deserialization, and the same shape as the
/// `ProfileScope::household()` default that PAI-1's own doc comment warns
/// about. `Serialize` is kept so a refusal can be logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DelegationDepth(u8);

impl DelegationDepth {
    /// A turn the user started.
    pub const ROOT: Self = DelegationDepth(0);

    pub fn get(self) -> u8 {
        self.0
    }

    pub fn is_root(self) -> bool {
        self.0 == 0
    }

    /// The depth one level down, or a refusal at the cap.
    ///
    /// Private on purpose: [`DelegationAuthority::delegate`] is the only caller,
    /// so there is no way for anything outside this module to mint a depth that
    /// was not derived from a real parent.
    fn deeper(self) -> Result<Self, DelegationRefused> {
        let next = self.0.saturating_add(1);
        if next > MAX_DELEGATION_DEPTH {
            return Err(DelegationRefused::DepthExceeded {
                requested: next,
                cap: MAX_DELEGATION_DEPTH,
            });
        }
        Ok(DelegationDepth(next))
    }
}

// ── Role ────────────────────────────────────────────────────────────────────

/// What a role may do with the speaker's personal data.
///
/// **There is no variant that widens.** That is the whole design: the child's
/// profile scope is a function of the parent's, and this enum's codomain
/// contains only the parent's own scope and [`ProfileScope::Guest`]. A role
/// cannot ask to run as `Household`, and it cannot ask to run as a named owner
/// — which would be meaningless anyway, since which member is speaking is a
/// per-turn fact and a role is a stored config.
///
/// `Inherit` is the serde default, and that is safe for the same reason: it
/// produces the parent's scope exactly, never more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RolePersonalData {
    /// Run with the parent's scope, unchanged.
    #[default]
    Inherit,
    /// Run with no personal data at all, whatever the parent may reach.
    Deny,
}

impl RolePersonalData {
    /// Every variant, in the same shape as [`RedactionKind::ALL`] and
    /// [`OnboardingStep::ALL`] elsewhere in this crate.
    ///
    /// It exists so a guard can iterate the ENUM rather than an array literal
    /// of today's variants. That distinction is not cosmetic: the scope guard
    /// used to loop over `[Inherit, Deny]` written out by hand, so a third
    /// variant whose [`narrow`](Self::narrow) arm returned
    /// [`ProfileScope::Household`] compiled, widened a `Guest` parent to the
    /// whole household, and failed nothing.
    ///
    /// A list can go stale, so it is not trusted on its own —
    /// `all_lists_every_variant_the_enum_actually_has` derives the real variant
    /// set from the enum's own `Deserialize` impl and fails if this array is
    /// missing one.
    ///
    /// [`RedactionKind::ALL`]: crate::security::domain::redaction::RedactionKind::ALL
    /// [`OnboardingStep::ALL`]: crate::user_data::domain::onboarding::OnboardingStep::ALL
    pub const ALL: [RolePersonalData; 2] = [RolePersonalData::Inherit, RolePersonalData::Deny];

    /// The child's scope, given the parent's.
    ///
    /// Guaranteed by construction to satisfy `result.is_within(parent)`.
    ///
    /// This `match` is exhaustive, so a new variant breaks the build here. The
    /// arm you are about to write must also be added to [`ALL`](Self::ALL), or
    /// every scope guard in this module will keep iterating a set that does not
    /// contain it.
    fn narrow(self, parent: &ProfileScope) -> ProfileScope {
        match self {
            RolePersonalData::Inherit => parent.clone(),
            RolePersonalData::Deny => ProfileScope::Guest,
        }
    }
}

/// A named, reusable agent persona with hard limits.
///
/// Fields are private and the constructor validates, so an `AgentRole` that
/// exists is one whose limits are inside the caps. There is no way to hold a
/// role asking for 500 turns.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRole {
    name: String,
    instructions: String,
    tool_groups: BTreeSet<String>,
    personal_data: RolePersonalData,
    max_turns: u32,
    context_fraction: f32,
}

impl AgentRole {
    /// Build and validate a role.
    pub fn new(
        name: impl Into<String>,
        instructions: impl Into<String>,
        tool_groups: BTreeSet<String>,
        personal_data: RolePersonalData,
        max_turns: u32,
        context_fraction: f32,
    ) -> Result<Self, RoleError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(RoleError::MissingName);
        }
        let instructions = instructions.into();
        if instructions.trim().is_empty() {
            return Err(RoleError::MissingInstructions { role: name });
        }
        if max_turns == 0 || max_turns > MAX_ROLE_MAX_TURNS {
            return Err(RoleError::MaxTurnsOutOfRange {
                role: name,
                requested: max_turns,
                cap: MAX_ROLE_MAX_TURNS,
            });
        }
        if !(context_fraction.is_finite() && context_fraction > 0.0 && context_fraction <= 1.0) {
            return Err(RoleError::ContextFractionOutOfRange {
                role: name,
                requested: context_fraction,
            });
        }
        Ok(Self {
            name,
            instructions,
            tool_groups,
            personal_data,
            max_turns,
            context_fraction,
        })
    }

    /// Read a role out of a recipe's YAML.
    ///
    /// Returns `Ok(None)` when the recipe carries no [`ROLE_YAML_KEY`] block —
    /// an ordinary routine, which is what every recipe in every pond is today.
    ///
    /// Returns `Err` when the block is PRESENT and malformed. That direction is
    /// deliberate and it is the one that matters: the alternative — falling
    /// back to a default role — would hand a child whatever the default tool
    /// set is on exactly the input a human got wrong, which is the widening
    /// failure path. A role that will not parse does not exist, and a
    /// delegation naming it fails to find it.
    ///
    /// `tool_groups` is REQUIRED inside the block and the block is
    /// `deny_unknown_fields`, so `toolgroups:` or `tool_group:` is a parse
    /// error rather than an absent narrowing.
    pub fn from_recipe_yaml(recipe_name: &str, yaml: &str) -> Result<Option<Self>, RoleError> {
        let doc: RecipeRoleDocument = serde_yaml::from_str(yaml).map_err(|e| RoleError::Yaml {
            role: recipe_name.to_string(),
            message: e.to_string(),
        })?;
        let Some(block) = doc.giap_role else {
            return Ok(None);
        };
        let instructions = block
            .instructions
            .or(doc.instructions)
            .or(doc.prompt)
            .unwrap_or_default();
        Self::new(
            recipe_name,
            instructions,
            block.tool_groups.into_iter().collect(),
            block.personal_data,
            block.max_turns,
            block.context_fraction,
        )
        .map(Some)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// The groups this role WANTS. Not what a child gets — that is this
    /// intersected with the parent's set, in [`DelegationAuthority::delegate`].
    pub fn requested_tool_groups(&self) -> &BTreeSet<String> {
        &self.tool_groups
    }

    pub fn personal_data(&self) -> RolePersonalData {
        self.personal_data
    }

    pub fn max_turns(&self) -> u32 {
        self.max_turns
    }

    pub fn context_fraction(&self) -> f32 {
        self.context_fraction
    }
}

/// The top-level shape `from_recipe_yaml` reads. Deliberately NOT
/// `deny_unknown_fields`: a recipe legally carries Goose's own fields
/// (`title`, `description`, `extensions`, `settings`, …) and refusing to read a
/// role because the recipe also has a title would be absurd.
#[derive(Debug, Deserialize)]
struct RecipeRoleDocument {
    #[serde(default)]
    giap_role: Option<RoleBlock>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
}

/// GIAP's own block, which IS `deny_unknown_fields` — inside a key we own, an
/// unrecognised field is a typo, and the typo that matters most is one on
/// `tool_groups`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleBlock {
    /// Required. There is no default, because every default is either "no
    /// tools" (which reads as a bug) or "the parent's tools" (which is the
    /// scope-widening default PAI-6 exists to avoid).
    tool_groups: Vec<String>,
    #[serde(default)]
    personal_data: RolePersonalData,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default = "default_role_max_turns")]
    max_turns: u32,
    #[serde(default = "default_role_context_fraction")]
    context_fraction: f32,
}

fn default_role_max_turns() -> u32 {
    DEFAULT_ROLE_MAX_TURNS
}

fn default_role_context_fraction() -> f32 {
    DEFAULT_CONTEXT_FRACTION
}

/// Why a stored role could not be read.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum RoleError {
    #[error("a role must have a name")]
    MissingName,
    #[error("role `{role}` has no instructions, and its recipe has no prompt either")]
    MissingInstructions { role: String },
    #[error("role `{role}` asks for {requested} turns; the cap is {cap}")]
    MaxTurnsOutOfRange {
        role: String,
        requested: u32,
        cap: u32,
    },
    #[error("role `{role}` asks for a context fraction of {requested}; it must be in (0.0, 1.0]")]
    ContextFractionOutOfRange { role: String, requested: f32 },
    #[error("role `{role}` has an unreadable `{ROLE_YAML_KEY}` block: {message}")]
    Yaml { role: String, message: String },
}

// ── The request, the authority, and the spec ────────────────────────────────

/// What the model asks for when it calls `delegate`.
///
/// **Every field a model could use to widen its own authority is absent, and
/// `deny_unknown_fields` makes that absence enforced rather than ignored.**
/// There is no `profile_scope`, no `tool_groups`, no `depth`, no `session_id`.
/// A model that emits one gets a parse error it can see and correct, instead of
/// a silently-dropped field that makes the prompt look like it worked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRequest {
    /// Which stored role to run. Must match an [`AgentRole::name`].
    pub role: String,
    /// What this particular child is being asked to do.
    pub instructions: String,
    /// Opaque structured inputs, passed through to the child's prompt.
    #[serde(default)]
    pub inputs: serde_json::Value,
}

/// What a running turn is authorised to do, and therefore the ceiling on what
/// anything it delegates to may do.
///
/// Constructed by [`root`](Self::root) at the edge, once per turn, from a scope
/// that PAI-1's `identity_resolution::resolve` already decided; or by
/// [`TaskSpec::child_authority`] for a subagent, which is the only other
/// producer in the crate. There is no `Default`, no `Deserialize` and no
/// field-wise constructor, so an authority cannot be assembled out of nothing
/// by a later refactor that "just needs one for a test".
#[derive(Debug, Clone)]
pub struct DelegationAuthority {
    session_id: String,
    scope: ProfileScope,
    tool_groups: BTreeSet<String>,
    depth: DelegationDepth,
}

impl DelegationAuthority {
    /// The authority of a turn the user started.
    ///
    /// `tool_groups` is what this session actually got — the post-selection,
    /// post-guest-subtraction set, not the catalog. Passing the catalog here
    /// would make every subsequent intersection a no-op, which is the shape
    /// PAI-1 P5 shipped and had to repair.
    ///
    /// # Why this stays `pub` even though its only argument is a string
    ///
    /// It takes a session id as a bare string and stamps
    /// [`DelegationDepth::ROOT`] on it, and nothing in the tree can answer "is
    /// this session a subagent's?" — no migration carries a parent, depth or
    /// subagent column. So on its face anything in the workspace can mint an
    /// authority claiming to be a root turn.
    ///
    /// What makes that safe is that an authority is a **ceiling, not a key**.
    /// P3 closed the live path from the other end: the edge builds the turn's
    /// authority inside the turn with [`for_turn`](Self::for_turn), and
    /// `TurnAuthorityRegistry` refuses a spawn whose parent turn is not live, so
    /// a hand-made authority opens no door. And what a forged one could still
    /// ask for is bounded here —
    /// `forged_authority_tests::the_widest_authority_anyone_could_forge_still_cannot_widen_a_child`
    /// mints the widest authority expressible and shows the child it produces
    /// still loses every group no subagent may hold and still cannot delegate
    /// again.
    pub fn root(
        session_id: impl Into<String>,
        scope: ProfileScope,
        tool_groups: BTreeSet<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            scope,
            tool_groups,
            depth: DelegationDepth::ROOT,
        }
    }

    /// The authority of a turn, built from the exact tool names that turn was
    /// given.
    ///
    /// PAI-6 P3. This is what the edge calls, and taking TOOL NAMES rather than
    /// group names is the point: the adapter holds one set — the
    /// post-selection, post-guest-subtraction allow-set it publishes to the
    /// provider shim — and this derives the groups from that same set rather
    /// than letting the caller assemble a second list that could disagree.
    /// Passing the catalog, or the pre-subtraction set, would make every
    /// downstream intersection a no-op; that is the shape PAI-1 P5 shipped and
    /// had to repair, and it is the single easiest way to make this whole
    /// workstream inert.
    ///
    /// An unprefixed name (Goose plumbing: the final-output tool, platform
    /// tools) belongs to no group and is dropped, exactly as
    /// [`TaskSpec::grants_tool`] would deny it.
    pub fn for_turn<'a>(
        session_id: impl Into<String>,
        scope: ProfileScope,
        allowed_tools: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let tool_groups: BTreeSet<String> = allowed_tools
            .into_iter()
            .filter_map(|tool| group_of_tool(tool).map(str::to_string))
            .collect();
        Self::root(session_id, scope, tool_groups)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn profile_scope(&self) -> &ProfileScope {
        &self.scope
    }

    pub fn tool_groups(&self) -> &BTreeSet<String> {
        &self.tool_groups
    }

    pub fn depth(&self) -> DelegationDepth {
        self.depth
    }

    /// Whether this turn is allowed to delegate at all.
    ///
    /// The intended use is to decide whether to OFFER the `delegate` tool: a
    /// subagent that cannot see the tool cannot try to call it, which is
    /// cheaper on a small model than a refusal it has to read and understand.
    /// It is not the enforcement — [`delegate`](Self::delegate) is.
    pub fn may_delegate(&self) -> bool {
        self.depth.deeper().is_ok()
    }

    /// Derive a child's task from this turn's authority and a stored role.
    ///
    /// The single narrowing point. Everything the child may do is computed
    /// here, from the parent and the role; nothing is taken from `request`
    /// except the words.
    pub fn delegate(
        &self,
        role: &AgentRole,
        request: TaskRequest,
    ) -> Result<TaskSpec, DelegationRefused> {
        if request.role != role.name {
            return Err(DelegationRefused::RoleMismatch {
                requested: request.role,
                supplied: role.name.clone(),
            });
        }
        let instructions = request.instructions.trim().to_string();
        if instructions.is_empty() {
            return Err(DelegationRefused::EmptyInstructions {
                role: role.name.clone(),
            });
        }
        // The two refusals above are about the REQUEST being malformed; this
        // one is about the authority. Ordering them this way means a subagent
        // that sends nonsense is told the nonsense, not the depth cap -- and
        // either way nothing is constructed.
        let depth = self.depth.deeper()?;
        let scope = role.personal_data.narrow(&self.scope);
        // `&scope`, not `&self.scope`. The subtraction is keyed on the CHILD's
        // scope, and the only case where the two differ is the exact one this
        // exists for: a Household parent running a `personal_data: deny` role.
        // Pass the parent's and that case -- a Guest-scoped child still holding
        // `giap-memory` -- is precisely what survives.
        let tool_groups = narrow_child_groups(&role.tool_groups, &self.tool_groups, &scope);
        Ok(TaskSpec {
            id: uuid::Uuid::new_v4().to_string(),
            role: role.name.clone(),
            instructions,
            inputs: request.inputs,
            parent_session_id: self.session_id.clone(),
            scope,
            tool_groups,
            depth,
            max_turns: role.max_turns,
            context_fraction: role.context_fraction,
        })
    }
}

/// The child's tool groups: the intersection, minus what the child's own scope
/// denies, minus what no subagent may hold.
///
/// PAI-6 P3, and the whole of it for the tool axis. Three subtractions, in this
/// order, and every one of them can only ever remove:
///
/// 1. **Intersect with the parent.** Invariant 1. The parent's set is the
///    turn's real post-selection, post-guest-subtraction allow-set (see
///    [`DelegationAuthority::for_turn`]), so this is a ceiling and not a
///    formality.
/// 2. **Subtract what the CHILD's scope denies.** This is the subtraction the
///    obvious implementation misses, and it is where the profile axis and the
///    tool axis meet. A `Household` parent running a role with
///    `personal_data: deny` produces a `Guest` child — and without this line
///    that Guest child keeps `giap-memory`, whose MCP tools carry no session
///    and read the household's memory regardless. A scope that says Guest while
///    the tools say Household is not a narrowing, it is a laundering route.
/// 3. **Subtract [`groups_denied_to_subagents`].** Withheld from every child at
///    every scope, because the controls those groups depend on cannot see a
///    subagent at all.
fn narrow_child_groups(
    requested: &BTreeSet<String>,
    parent: &BTreeSet<String>,
    child_scope: &ProfileScope,
) -> BTreeSet<String> {
    let mut groups: BTreeSet<String> = requested.intersection(parent).cloned().collect();
    if child_scope.excludes_everything() {
        for denied in groups_denied_to_guests() {
            groups.remove(*denied);
        }
    }
    for denied in groups_denied_to_subagents() {
        groups.remove(*denied);
    }
    groups
}

/// A child agent run that has been authorised but not yet started.
///
/// Every field is private and derived. There is no constructor, no `Default`
/// and no `Deserialize`: the only way to obtain one is
/// [`DelegationAuthority::delegate`], which means the only way to obtain one is
/// to already hold the parent's authority.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    id: String,
    role: String,
    instructions: String,
    inputs: serde_json::Value,
    parent_session_id: String,
    scope: ProfileScope,
    tool_groups: BTreeSet<String>,
    depth: DelegationDepth,
    max_turns: u32,
    context_fraction: f32,
}

impl TaskSpec {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    pub fn inputs(&self) -> &serde_json::Value {
        &self.inputs
    }

    pub fn parent_session_id(&self) -> &str {
        &self.parent_session_id
    }

    pub fn profile_scope(&self) -> &ProfileScope {
        &self.scope
    }

    /// The groups the child may use. **Empty means empty.**
    ///
    /// Goose's `ExtensionConfig::available_tools` treats an empty vector as
    /// "all tools" (`extension.rs`, `available_tools.is_empty() || contains`),
    /// and GIAP's own `builtin_extension_config` helper passes `vec![]`. An
    /// adapter that forwards this set straight into that field, having narrowed
    /// it to nothing, would grant the child everything. Use
    /// [`grants_tool`](Self::grants_tool) instead of reasoning about the set.
    pub fn tool_groups(&self) -> &BTreeSet<String> {
        &self.tool_groups
    }

    pub fn depth(&self) -> DelegationDepth {
        self.depth
    }

    pub fn max_turns(&self) -> u32 {
        self.max_turns
    }

    pub fn context_fraction(&self) -> f32 {
        self.context_fraction
    }

    /// May the child call this fully-qualified tool?
    ///
    /// Stricter than `tool_selection::filter_tools_by_groups`, and the
    /// difference is deliberate. That function KEEPS an unprefixed tool (goose
    /// plumbing) and a tool from a non-catalog extension (a user-added MCP
    /// server), because for the user's own turn those were added on purpose.
    /// For a child the same reasoning inverts: the child was given a named,
    /// narrow set, and anything that does not resolve into that set is denied.
    /// Unknown means no.
    pub fn grants_tool(&self, tool_name: &str) -> bool {
        match group_of_tool(tool_name) {
            Some(extension) => self.tool_groups.contains(extension),
            None => false,
        }
    }

    /// The authority the child itself runs under, once its session exists.
    ///
    /// Taking the session id here rather than at [`DelegationAuthority::delegate`]
    /// is not tidiness: the child's engine session does not exist until the
    /// adapter creates it, and inventing an id in the domain would mean two
    /// places believing they knew it.
    ///
    /// This is the ONLY public producer of a non-root [`DelegationAuthority`],
    /// which is what makes [`DelegationDepth`] un-forgeable: a subagent is
    /// handed this, and has no way to construct one claiming to be shallower.
    pub fn child_authority(&self, child_session_id: impl Into<String>) -> DelegationAuthority {
        DelegationAuthority {
            session_id: child_session_id.into(),
            scope: self.scope.clone(),
            tool_groups: self.tool_groups.clone(),
            depth: self.depth,
        }
    }
}

/// Why a delegation was refused before it ever started.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum DelegationRefused {
    #[error("delegation depth {requested} exceeds the cap of {cap}")]
    DepthExceeded { requested: u8, cap: u8 },
    #[error("delegation asked for role `{requested}` but was given role `{supplied}`")]
    RoleMismatch { requested: String, supplied: String },
    #[error("delegation to role `{role}` carried no instructions")]
    EmptyInstructions { role: String },
    #[error("no role named `{0}` exists")]
    UnknownRole(String),
}

// ── The run ─────────────────────────────────────────────────────────────────

/// Where a child agent run got to.
///
/// [`Cancelled`](Self::Cancelled) and
/// [`TurnBudgetExhausted`](Self::TurnBudgetExhausted) exist as separate
/// variants because the engine cannot distinguish them from success on its own.
/// Goose's `run_subagent_task` returns `Ok(partial_text)` when the child is
/// cancelled — the reply loop simply breaks — and when `max_turns` is hit it
/// returns the literal `MAX_TURNS_MESSAGE` string ("I've reached the maximum
/// number of actions I can do without user input…") as the result. An
/// orchestrator that only inspects `Result` reports both as answers. The
/// adapter has to tell them apart and say so here; a test asserting "cancel
/// works" by checking for an `Err` would be vacuous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Authorised, waiting for a concurrency permit.
    Queued,
    Running,
    /// Finished on its own terms with an answer.
    Completed,
    /// Stopped by the parent, or by the parent itself being cancelled.
    Cancelled,
    /// Ran out of turns. Whatever text came back is a budget message, not an
    /// answer.
    TurnBudgetExhausted,
    Failed,
}

impl TaskStatus {
    /// No further transition is possible.
    pub fn is_terminal(self) -> bool {
        !matches!(self, TaskStatus::Queued | TaskStatus::Running)
    }

    /// Whether the run's text may be handed to the parent as a result.
    ///
    /// Only [`Completed`](Self::Completed). This is the predicate that stops a
    /// cancellation or a blown turn budget being reported to the user as an
    /// answer.
    pub fn produced_an_answer(self) -> bool {
        matches!(self, TaskStatus::Completed)
    }
}

/// A child agent run, as the parent sees it.
///
/// PAI-6 invariant 4: subagent conversations never enter the parent's
/// `session_messages`; only results do. This struct IS "only results" — there
/// is nowhere on it to put a transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: String,
    pub role: String,
    pub parent_session_id: String,
    pub status: TaskStatus,
    /// The child's answer. Present only for a run that actually produced one —
    /// read it through [`result_for_parent`](Self::result_for_parent).
    pub result: Option<String>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl TaskRun {
    /// Start a run from an authorised spec.
    pub fn started(spec: &TaskSpec, at: DateTime<Utc>) -> Self {
        Self {
            id: spec.id().to_string(),
            role: spec.role().to_string(),
            parent_session_id: spec.parent_session_id().to_string(),
            status: TaskStatus::Running,
            result: None,
            error: None,
            started_at: at,
            finished_at: None,
        }
    }

    /// The text the parent may see, or `None`.
    ///
    /// Guards the `Ok`-on-cancel and `Ok`-on-budget-exhaustion shapes described
    /// on [`TaskStatus`]: a `result` set alongside a non-`Completed` status is
    /// not an answer, whatever the engine returned.
    pub fn result_for_parent(&self) -> Option<&str> {
        if self.status.produced_an_answer() {
            self.result.as_deref()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod depth_tests {
    use super::*;

    #[test]
    fn a_root_turn_may_delegate_and_its_child_may_not() {
        let root = DelegationAuthority::root(
            "s1",
            ProfileScope::Household,
            ["giap-weather".to_string()].into_iter().collect(),
        );
        assert!(root.may_delegate());
        assert!(root.depth().is_root());

        let role = weather_role();
        let spec = root
            .delegate(&role, request("researcher", "look it up"))
            .unwrap();
        assert_eq!(spec.depth().get(), 1);

        let child = spec.child_authority("s1-child");
        assert!(
            !child.may_delegate(),
            "a subagent must not be able to spawn another"
        );
        let refused = child
            .delegate(&role, request("researcher", "and again"))
            .unwrap_err();
        assert_eq!(
            refused,
            DelegationRefused::DepthExceeded {
                requested: 2,
                cap: MAX_DELEGATION_DEPTH
            }
        );
    }

    /// Vacuity control for the test above: the refusal must come from the DEPTH,
    /// not from the child having been left with no tools or a Guest scope. Give
    /// the child a scope and a tool set as wide as the root's and it must still
    /// be refused.
    #[test]
    fn a_child_is_refused_on_depth_even_when_nothing_else_narrowed() {
        let groups: BTreeSet<String> = ["giap-weather".to_string()].into_iter().collect();
        let root = DelegationAuthority::root("s1", ProfileScope::Household, groups.clone());
        let role = weather_role();
        let spec = root.delegate(&role, request("researcher", "go")).unwrap();
        let child = spec.child_authority("s1-child");

        assert_eq!(child.profile_scope(), &ProfileScope::Household);
        assert_eq!(child.tool_groups(), &groups);
        assert!(matches!(
            child.delegate(&role, request("researcher", "go")),
            Err(DelegationRefused::DepthExceeded { .. })
        ));
    }

    /// The cap is the number the refusal is built from, so a change to
    /// `MAX_DELEGATION_DEPTH` must be a deliberate edit here too.
    #[test]
    fn the_depth_cap_is_one() {
        assert_eq!(MAX_DELEGATION_DEPTH, 1);
    }

    fn weather_role() -> AgentRole {
        AgentRole::new(
            "researcher",
            "Answer the question and stop.",
            ["giap-weather".to_string()].into_iter().collect(),
            RolePersonalData::Inherit,
            3,
            0.4,
        )
        .unwrap()
    }

    fn request(role: &str, instructions: &str) -> TaskRequest {
        TaskRequest {
            role: role.to_string(),
            instructions: instructions.to_string(),
            inputs: serde_json::Value::Null,
        }
    }
}

#[cfg(test)]
mod forged_authority_tests {
    use super::*;
    use crate::mcp::domain::tool_group::TOOL_GROUPS;

    fn request() -> TaskRequest {
        TaskRequest {
            role: "r".to_string(),
            instructions: "go".to_string(),
            inputs: serde_json::Value::Null,
        }
    }

    /// [`DelegationAuthority::root`] is a public depth-0 constructor keyed only
    /// on a session-id string, and nothing in this tree can answer "is that
    /// session a subagent's?" — no migration carries a parent, depth or
    /// subagent column. So anything in the workspace can hand itself an
    /// authority that claims to be a root turn.
    ///
    /// It stays `pub` — pond-adapters-goose's orchestrator tests construct one
    /// — and this is the test that says why that is now safe. Two reasons, and
    /// only the second is testable from `pond-core`:
    ///
    /// 1. P3 closed the live path from the other end. The edge builds the
    ///    turn's authority inside the turn via
    ///    [`DelegationAuthority::for_turn`], and `TurnAuthorityRegistry` refuses
    ///    a spawn whose parent turn is not live, so a hand-made authority is not
    ///    a key to anything.
    /// 2. An authority is a **ceiling, not a capability**. What is asserted
    ///    below: mint the widest one expressible — the entire tool catalog, the
    ///    whole household, depth 0 — and the child it can produce still loses
    ///    every group no subagent may hold, and still cannot delegate again.
    #[test]
    fn the_widest_authority_anyone_could_forge_still_cannot_widen_a_child() {
        let every_group: BTreeSet<String> = TOOL_GROUPS
            .iter()
            .map(|g| g.extension.to_string())
            .collect();
        let forged = DelegationAuthority::root(
            "a-session-id-that-nobody-checked",
            ProfileScope::Household,
            every_group.clone(),
        );
        let role = AgentRole::new(
            "r",
            "go",
            every_group.clone(),
            RolePersonalData::Inherit,
            3,
            0.5,
        )
        .unwrap();
        let spec = forged.delegate(&role, request()).unwrap();

        for denied in groups_denied_to_subagents() {
            assert!(
                !spec.tool_groups().contains(*denied),
                "a forged root authority handed a child `{denied}`, which no subagent may hold"
            );
            assert!(!spec.grants_tool(&format!("{denied}__anything")));
        }

        // The forged root spends its one level immediately, so the tree it can
        // start is one deep however it was constructed.
        assert_eq!(spec.depth().get(), 1);
        let child = spec.child_authority("child");
        assert!(!child.may_delegate());
        assert!(matches!(
            child.delegate(&role, request()),
            Err(DelegationRefused::DepthExceeded { .. })
        ));

        // Vacuity control: the delegation really happened and really granted
        // something, so the refusals above are about the denylist and the depth
        // cap rather than about nothing having been produced.
        assert!(spec.grants_tool("giap-weather__get_forecast"));
        assert!(!spec.tool_groups().is_empty());
    }
}

#[cfg(test)]
mod scope_inheritance_tests {
    use super::*;

    fn role(personal_data: RolePersonalData) -> AgentRole {
        AgentRole::new(
            "r",
            "do the thing",
            ["giap-weather".to_string()].into_iter().collect(),
            personal_data,
            3,
            0.5,
        )
        .unwrap()
    }

    fn request() -> TaskRequest {
        TaskRequest {
            role: "r".to_string(),
            instructions: "go".to_string(),
            inputs: serde_json::Value::Null,
        }
    }

    fn parents() -> Vec<ProfileScope> {
        vec![
            ProfileScope::Guest,
            ProfileScope::Owner("jerry".into()),
            ProfileScope::Household,
        ]
    }

    /// `RolePersonalData::ALL` is what every scope guard in this module
    /// iterates, so a variant missing from it is a variant nothing checks for
    /// widening. An array literal cannot see a variant added tomorrow, and a
    /// hand-written second list is just another array literal — so derive the
    /// truth from the enum itself. serde's `unknown variant` error names every
    /// variant the `Deserialize` impl knows about, and that impl is generated
    /// from the enum, so a new variant turns up here without anyone
    /// remembering this file.
    #[test]
    fn all_lists_every_variant_the_enum_actually_has() {
        let message = serde_json::from_str::<RolePersonalData>("\"definitely_not_a_variant\"")
            .expect_err("that is not a variant")
            .to_string();
        // `unknown variant `x`, expected one of `inherit`, `deny`` for three or
        // more, and `expected `inherit` or `deny`` for two. Both shapes are a
        // run of backticked names whose first entry is the bad one.
        let from_the_enum: Vec<&str> = message.split('`').skip(1).step_by(2).skip(1).collect();
        assert!(
            !from_the_enum.is_empty(),
            "no variant names could be read out of serde's message {message:?}, so this test \
             can no longer see a new variant at all"
        );
        let listed: Vec<String> = RolePersonalData::ALL
            .iter()
            .map(|v| {
                serde_json::to_string(v)
                    .expect("a unit variant serializes")
                    .trim_matches('"')
                    .to_string()
            })
            .collect();
        for variant in &from_the_enum {
            assert!(
                listed.iter().any(|l| l == variant),
                "RolePersonalData has a variant `{variant}` that RolePersonalData::ALL does not \
                 list. Every scope guard in this module iterates ALL, so an unlisted variant is \
                 one that nothing tests for widening"
            );
        }
        assert_eq!(
            from_the_enum.len(),
            listed.len(),
            "RolePersonalData::ALL and the enum disagree on how many variants exist: \
             {from_the_enum:?} against {listed:?}"
        );
    }

    /// PAI-6 invariant 1, for the profile axis. Exhaustive over both the enum
    /// and the three scope shapes, and the enum half is exhaustive because it
    /// iterates [`RolePersonalData::ALL`] rather than an array literal of the
    /// variants that happened to exist the day it was written — which is what
    /// it used to do, and which is why a third variant returning
    /// `ProfileScope::Household` could widen a Guest parent and fail nothing.
    #[test]
    fn no_role_setting_can_widen_the_parents_scope() {
        for personal_data in RolePersonalData::ALL {
            for parent_scope in parents() {
                let parent = DelegationAuthority::root(
                    "s1",
                    parent_scope.clone(),
                    ["giap-weather".to_string()].into_iter().collect(),
                );
                let spec = parent.delegate(&role(personal_data), request()).unwrap();
                assert!(
                    spec.profile_scope().is_within(&parent_scope),
                    "role setting {personal_data:?} widened {parent_scope:?} to {:?}",
                    spec.profile_scope()
                );
            }
        }
    }

    /// Vacuity control for the test above. `is_within` is reflexive, so a
    /// derivation that simply copied the parent would pass it — assert that
    /// `Deny` genuinely MOVES, and that `Inherit` genuinely does not.
    #[test]
    fn deny_actually_drops_to_guest_and_inherit_actually_copies() {
        let parent = DelegationAuthority::root(
            "s1",
            ProfileScope::Owner("jerry".into()),
            ["giap-weather".to_string()].into_iter().collect(),
        );
        let inherited = parent
            .delegate(&role(RolePersonalData::Inherit), request())
            .unwrap();
        assert_eq!(
            inherited.profile_scope(),
            &ProfileScope::Owner("jerry".into())
        );

        let denied = parent
            .delegate(&role(RolePersonalData::Deny), request())
            .unwrap();
        assert_eq!(denied.profile_scope(), &ProfileScope::Guest);
    }

    /// A guest parent may not launder access through a subagent.
    #[test]
    fn a_guest_parent_produces_a_guest_child() {
        let parent = DelegationAuthority::root(
            "s1",
            ProfileScope::Guest,
            ["giap-weather".to_string()].into_iter().collect(),
        );
        for personal_data in RolePersonalData::ALL {
            let spec = parent.delegate(&role(personal_data), request()).unwrap();
            assert_eq!(spec.profile_scope(), &ProfileScope::Guest);
        }
    }

    /// The narrowing happens in `delegate`, but the value a running child
    /// actually holds comes from `TaskSpec::child_authority` — a second copy,
    /// and therefore a second chance to widen.
    ///
    /// The only test that read `child_authority(...).profile_scope()` used a
    /// HOUSEHOLD parent, and Household is the widest scope there is, so its
    /// fixture could not tell a copied scope from a hardcoded maximal one:
    /// replacing `self.scope.clone()` with `ProfileScope::Household` handed
    /// every subagent the whole household and left the suite green. Every
    /// parent shape here is one that answer fails, and the claim is stated with
    /// [`ProfileScope::is_within`] rather than by writing the lattice out again.
    #[test]
    fn a_childs_own_authority_is_the_specs_and_never_wider() {
        // Deliberately wider than the role asks for, so the tool assertion
        // below can tell "the spec's set" from "the parent's set".
        let parent_groups: BTreeSet<String> = ["giap-weather".to_string(), "giap-news".to_string()]
            .into_iter()
            .collect();
        for personal_data in RolePersonalData::ALL {
            for parent_scope in parents() {
                let parent =
                    DelegationAuthority::root("s1", parent_scope.clone(), parent_groups.clone());
                let spec = parent.delegate(&role(personal_data), request()).unwrap();
                let child = spec.child_authority("s1-child");

                assert_eq!(
                    child.profile_scope(),
                    spec.profile_scope(),
                    "a {personal_data:?} child of a {parent_scope:?} parent runs under a scope \
                     its own spec never granted"
                );
                assert!(
                    child.profile_scope().is_within(&parent_scope),
                    "child_authority widened {parent_scope:?} to {:?}",
                    child.profile_scope()
                );
                assert_eq!(
                    child.tool_groups(),
                    spec.tool_groups(),
                    "a child's authority holds tools its spec did not"
                );
                assert!(!child.tool_groups().contains("giap-news"));
                assert_eq!(child.depth(), spec.depth());
                assert_eq!(child.session_id(), "s1-child");
            }
        }
    }
}

#[cfg(test)]
mod tool_narrowing_tests {
    use super::*;

    fn groups(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn role_wanting(names: &[&str]) -> AgentRole {
        AgentRole::new("r", "go", groups(names), RolePersonalData::Inherit, 3, 0.5).unwrap()
    }

    fn request() -> TaskRequest {
        TaskRequest {
            role: "r".to_string(),
            instructions: "go".to_string(),
            inputs: serde_json::Value::Null,
        }
    }

    fn parent(names: &[&str]) -> DelegationAuthority {
        DelegationAuthority::root("s1", ProfileScope::Household, groups(names))
    }

    /// PAI-6 invariant 1, for the tool axis. A role naming a group its parent
    /// does not hold gets the intersection -- not the union, and not "the
    /// parent's set" (which the design text says, and which would be a
    /// widening whenever the role was narrower).
    #[test]
    fn a_role_cannot_reach_a_group_its_parent_lacks() {
        let spec = parent(&["giap-weather"])
            .delegate(&role_wanting(&["giap-weather", "giap-memory"]), request())
            .unwrap();
        assert_eq!(spec.tool_groups(), &groups(&["giap-weather"]));
        assert!(!spec.grants_tool("giap-memory__recall_memories"));
        assert!(spec.grants_tool("giap-weather__get_forecast"));
    }

    /// The narrow direction has to work too, or "intersection" is really "the
    /// parent's set" and the whole feature is inert.
    #[test]
    fn a_role_narrower_than_its_parent_keeps_only_what_it_asked_for() {
        let spec = parent(&["giap-weather", "giap-memory", "giap-news"])
            .delegate(&role_wanting(&["giap-weather"]), request())
            .unwrap();
        assert_eq!(spec.tool_groups(), &groups(&["giap-weather"]));
        assert!(!spec.grants_tool("giap-news__top_headlines"));
        assert!(!spec.grants_tool("giap-memory__recall_memories"));
    }

    /// The `available_tools: vec![]` trap, stated as a property of this type.
    /// Goose reads an empty allowlist as "everything"; here an empty set must
    /// grant nothing, or a role whose groups were all intersected away becomes
    /// the widest agent on the pond.
    #[test]
    fn an_empty_group_set_grants_nothing() {
        let spec = parent(&["giap-weather"])
            .delegate(&role_wanting(&["giap-news"]), request())
            .unwrap();
        assert!(spec.tool_groups().is_empty());
        for tool in [
            "giap-weather__get_forecast",
            "giap-memory__recall_memories",
            "giap-news__top_headlines",
        ] {
            assert!(!spec.grants_tool(tool), "empty set granted {tool}");
        }
    }

    /// An unprefixed name (goose plumbing) and an unknown prefix (a user-added
    /// MCP server) are both denied to a child, which is stricter than
    /// `filter_tools_by_groups` on purpose.
    #[test]
    fn unknown_shapes_are_denied_rather_than_kept() {
        let spec = parent(&["giap-weather"])
            .delegate(&role_wanting(&["giap-weather"]), request())
            .unwrap();
        assert!(!spec.grants_tool("platform__manage_schedule"));
        assert!(!spec.grants_tool("unprefixed"));
        assert!(!spec.grants_tool("some-user-mcp-server__do_it"));
    }

    /// PAI-6 P3, and the sharpest defect it fixes. `personal_data: deny` drops
    /// the child to `Guest`; before P3 the tool set was computed from the
    /// PARENT's scope, so a Household parent produced a Guest child still
    /// holding `giap-memory` -- whose MCP tools carry no session at all and
    /// read the household's memory regardless of what the scope says. A scope
    /// that says Guest while the tools say Household is a laundering route, not
    /// a narrowing.
    #[test]
    fn a_child_dropped_to_guest_loses_the_groups_a_guest_is_denied() {
        let spec = parent(&["giap-memory", "giap-weather", "giap-vision"])
            .delegate(
                &AgentRole::new(
                    "r",
                    "go",
                    groups(&["giap-memory", "giap-weather", "giap-vision"]),
                    RolePersonalData::Deny,
                    3,
                    0.5,
                )
                .unwrap(),
                request(),
            )
            .unwrap();

        assert_eq!(spec.profile_scope(), &ProfileScope::Guest);
        assert_eq!(spec.tool_groups(), &groups(&["giap-weather"]));
        assert!(!spec.grants_tool("giap-memory__recall_memories"));
        assert!(!spec.grants_tool("giap-memory__forget_memory"));
        assert!(!spec.grants_tool("giap-vision__who_was_seen"));
    }

    /// Vacuity control for the test above. The subtraction must be keyed on the
    /// CHILD's scope, not applied unconditionally: an `Inherit` role under a
    /// Household parent keeps `giap-memory`, or the previous test would pass
    /// against an implementation that simply deleted memory from every child.
    #[test]
    fn an_inheriting_child_of_a_household_parent_keeps_memory() {
        let spec = parent(&["giap-memory", "giap-weather"])
            .delegate(&role_wanting(&["giap-memory", "giap-weather"]), request())
            .unwrap();
        assert_eq!(spec.profile_scope(), &ProfileScope::Household);
        assert!(spec.grants_tool("giap-memory__recall_memories"));
    }

    /// Exhaustive over the denylist rather than naming two of it, so a group
    /// added there later is covered without anyone remembering this file.
    #[test]
    fn no_subagent_gets_a_group_the_subagent_denylist_names() {
        let denied: Vec<&str> = groups_denied_to_subagents().to_vec();
        let spec = parent(&denied)
            .delegate(&role_wanting(&denied), request())
            .unwrap();
        assert!(
            spec.tool_groups().is_empty(),
            "a role asked for the whole subagent denylist and kept {:?}",
            spec.tool_groups()
        );
        for group in &denied {
            assert!(
                !spec.grants_tool(&format!("{group}__anything")),
                "{group} is on the subagent denylist and reached a child anyway"
            );
        }
    }

    /// The draft half of PAI-6 P3, stated as the thing it actually prevents.
    /// `approve_draft` is gated by `is_draft_decision_permitted`, which
    /// resolves its actor through `engine_session_map` -- a table no subagent's
    /// engine session is in. The gate therefore answers
    /// `REASON_UNRESOLVED_ACTOR`, which under the DEFAULT `PolicyMode::Audit`
    /// PROCEEDS. Withholding the group is the enforcement.
    #[test]
    fn a_subagent_can_never_reach_a_draft_decision_tool() {
        for parent_scope in [
            ProfileScope::Household,
            ProfileScope::Owner("jerry".into()),
            ProfileScope::Guest,
        ] {
            let authority = DelegationAuthority::root(
                "s1",
                parent_scope.clone(),
                groups(&["giap-draft", "giap-weather"]),
            );
            for personal_data in RolePersonalData::ALL {
                let role = AgentRole::new(
                    "r",
                    "go",
                    groups(&["giap-draft", "giap-weather"]),
                    personal_data,
                    3,
                    0.5,
                )
                .unwrap();
                let spec = authority.delegate(&role, request()).unwrap();
                assert!(
                    !spec.grants_tool("giap-draft__approve_draft"),
                    "a {parent_scope:?} parent with {personal_data:?} handed a child the \
                     ability to approve staged actions"
                );
                assert!(!spec.grants_tool("giap-draft__reject_draft"));
                // Vacuity control, inline: the refusal is about draft, not
                // about the delegation having produced nothing at all.
                assert!(spec.grants_tool("giap-weather__get_forecast"));
            }
        }
    }

    /// `for_turn` is what the edge calls. Its input is the turn's real allow-set
    /// -- tool names -- and the groups it derives must be exactly the groups of
    /// those names, so that a guest subtraction the adapter already applied is
    /// carried through rather than undone.
    #[test]
    fn a_turn_authority_is_exactly_the_groups_of_the_tools_that_turn_held() {
        let authority = DelegationAuthority::for_turn(
            "s1",
            ProfileScope::Guest,
            [
                "giap-weather__get_forecast",
                "giap-weather__current_conditions",
                "giap-knowledge__lookup",
                // Goose plumbing: belongs to no group and must not become one.
                "final_output",
                "platform__manage_schedule",
            ],
        );
        assert_eq!(
            authority.tool_groups(),
            &groups(&["giap-weather", "giap-knowledge", "platform"])
        );
        assert!(!authority.tool_groups().contains("final_output"));
    }

    /// Every tool a full-surface turn could hold, one per group in the catalog.
    /// The point is that it is NOT hand-filtered: the guest fixture below is
    /// derived from this by the same function production derives it with.
    fn a_full_turns_tools() -> Vec<String> {
        use crate::mcp::domain::tool_group::{TOOL_GROUPS, TOOL_NAME_SEPARATOR};
        TOOL_GROUPS
            .iter()
            .map(|g| format!("{}{TOOL_NAME_SEPARATOR}a_tool", g.extension))
            .collect()
    }

    /// The failure this phase is most likely to ship: a turn whose allow-set was
    /// already guest-subtracted must not have `giap-memory` reappear in its
    /// authority.
    ///
    /// The fixture is built the way PRODUCTION builds it -- the full surface
    /// put through `subtract_guest_denied_tools`, the same call `chat_stream`
    /// makes at section 6d -- rather than by hand-writing a list that has
    /// already had memory taken out of it. This programme's `ProfileScope::Owner`
    /// incident was exactly a hand-built fixture that no code path could
    /// produce; a hand-built post-subtraction list here would assert that
    /// memory is absent from a list I removed it from myself.
    #[test]
    fn a_guest_turns_authority_cannot_contain_what_the_turn_was_denied() {
        let full = a_full_turns_tools();
        let guest_turn_tools =
            crate::mcp::services::tool_selection::subtract_guest_denied_tools(full.iter());
        let authority = DelegationAuthority::for_turn(
            "s1",
            ProfileScope::Guest,
            guest_turn_tools.iter().map(String::as_str),
        );
        for denied in groups_denied_to_guests() {
            assert!(
                !authority.tool_groups().contains(*denied),
                "{denied} was subtracted from the turn and reappeared in its authority"
            );
        }
        let spec = authority
            .delegate(&role_wanting(&["giap-memory", "giap-weather"]), request())
            .unwrap();
        assert!(!spec.grants_tool("giap-memory__recall_memories"));
        assert!(spec.grants_tool("giap-weather__get_forecast"));
    }

    /// Vacuity control for the test above, and the reason it is worth having:
    /// the SAME full surface, NOT guest-subtracted, does produce an authority
    /// holding `giap-memory`. So the assertion is about the subtraction being
    /// carried through, not about the catalog never having contained memory
    /// and not about `for_turn` dropping personal-data groups on its own --
    /// which it must not do, because a Household turn is entitled to them.
    #[test]
    fn the_same_surface_without_the_subtraction_does_carry_memory() {
        let full = a_full_turns_tools();
        let authority = DelegationAuthority::for_turn(
            "s1",
            ProfileScope::Household,
            full.iter().map(String::as_str),
        );
        for denied in groups_denied_to_guests() {
            assert!(
                authority.tool_groups().contains(*denied),
                "{denied} is missing from a FULL turn's authority, so the guest test next door \
                 proves nothing about the subtraction"
            );
        }
        let spec = authority
            .delegate(&role_wanting(&["giap-memory"]), request())
            .unwrap();
        assert!(spec.grants_tool("giap-memory__recall_memories"));
    }

    /// A typo in a stored role must lose the group, never gain one.
    #[test]
    fn a_mistyped_group_name_grants_nothing_extra() {
        let spec = parent(&["giap-weather", "giap-news"])
            .delegate(&role_wanting(&["giap-wether"]), request())
            .unwrap();
        assert!(spec.tool_groups().is_empty());
        assert!(!spec.grants_tool("giap-weather__get_forecast"));
    }
}

#[cfg(test)]
mod request_shape_tests {
    use super::*;

    /// The model supplies this. It must not be able to name its own scope, its
    /// own tools or its own depth -- and `deny_unknown_fields` turns "cannot
    /// name" from a comment into a parse error.
    #[test]
    fn a_request_carrying_an_authority_field_is_refused() {
        for hostile in [
            r#"{"role":"r","instructions":"go","profile_scope":"household"}"#,
            r#"{"role":"r","instructions":"go","tool_groups":["giap-memory"]}"#,
            r#"{"role":"r","instructions":"go","depth":0}"#,
            r#"{"role":"r","instructions":"go","session_id":"other"}"#,
        ] {
            let parsed: Result<TaskRequest, _> = serde_json::from_str(hostile);
            assert!(
                parsed.is_err(),
                "a delegate call was allowed to carry its own authority: {hostile}"
            );
        }
    }

    /// Vacuity control: the refusals above must be about the extra field, not
    /// about the payload being unparseable in general.
    #[test]
    fn an_ordinary_request_still_parses() {
        let ok: TaskRequest =
            serde_json::from_str(r#"{"role":"r","instructions":"look up the weather"}"#).unwrap();
        assert_eq!(ok.role, "r");
        assert_eq!(ok.inputs, serde_json::Value::Null);
    }

    #[test]
    fn a_request_with_no_instructions_is_refused_at_delegation() {
        let parent = DelegationAuthority::root(
            "s1",
            ProfileScope::Household,
            ["giap-weather".to_string()].into_iter().collect(),
        );
        let role = AgentRole::new(
            "r",
            "go",
            ["giap-weather".to_string()].into_iter().collect(),
            RolePersonalData::Inherit,
            3,
            0.5,
        )
        .unwrap();
        let refused = parent
            .delegate(
                &role,
                TaskRequest {
                    role: "r".into(),
                    instructions: "   ".into(),
                    inputs: serde_json::Value::Null,
                },
            )
            .unwrap_err();
        assert_eq!(
            refused,
            DelegationRefused::EmptyInstructions { role: "r".into() }
        );
    }

    #[test]
    fn a_request_naming_a_different_role_than_the_one_supplied_is_refused() {
        let parent = DelegationAuthority::root("s1", ProfileScope::Household, BTreeSet::new());
        let role = AgentRole::new(
            "researcher",
            "go",
            BTreeSet::new(),
            RolePersonalData::Inherit,
            3,
            0.5,
        )
        .unwrap();
        let refused = parent
            .delegate(
                &role,
                TaskRequest {
                    role: "housekeeper".into(),
                    instructions: "go".into(),
                    inputs: serde_json::Value::Null,
                },
            )
            .unwrap_err();
        assert!(matches!(refused, DelegationRefused::RoleMismatch { .. }));
    }
}

#[cfg(test)]
mod role_yaml_tests {
    use super::*;

    const ROLE_YAML: &str = r#"
title: Researcher
description: Looks things up
instructions: Answer the question from the tools you have, then stop.
giap_role:
  tool_groups:
    - giap-knowledge
    - giap-weather
  personal_data: deny
  max_turns: 4
  context_fraction: 0.3
"#;

    #[test]
    fn a_role_block_is_read_out_of_an_ordinary_recipe() {
        let role = AgentRole::from_recipe_yaml("researcher", ROLE_YAML)
            .unwrap()
            .expect("this recipe declares a role");
        assert_eq!(role.name(), "researcher");
        assert_eq!(
            role.requested_tool_groups(),
            &["giap-knowledge".to_string(), "giap-weather".to_string()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        assert_eq!(role.personal_data(), RolePersonalData::Deny);
        assert_eq!(role.max_turns(), 4);
        assert!((role.context_fraction() - 0.3).abs() < f32::EPSILON);
        assert_eq!(
            role.instructions(),
            "Answer the question from the tools you have, then stop."
        );
    }

    /// Every recipe in every pond today is a plain routine. Reading one must
    /// not invent a role for it.
    #[test]
    fn a_recipe_without_the_block_is_not_a_role() {
        let plain = "title: Morning Brief\nprompt: Give me the weather.\n";
        assert_eq!(
            AgentRole::from_recipe_yaml("morning_brief", plain).unwrap(),
            None
        );
    }

    /// The failure direction that matters. A malformed narrowing block must
    /// refuse, because the alternative -- defaulting -- hands a child a tool set
    /// nobody chose, on exactly the input a human got wrong.
    #[test]
    fn a_malformed_block_refuses_rather_than_defaulting() {
        let cases = [
            // `tool_groups` is required: absent means "no narrowing stated",
            // and there is no safe default for that.
            "giap_role:\n  personal_data: deny\n",
            // A typo on the one field that does the narrowing.
            "giap_role:\n  toolgroups:\n    - giap-weather\n",
            // Wrong type.
            "giap_role:\n  tool_groups: giap-weather\n",
            // Not YAML at all.
            "giap_role:\n  tool_groups: [\n",
        ];
        for yaml in cases {
            let full = format!("instructions: go\n{yaml}");
            let parsed = AgentRole::from_recipe_yaml("r", &full);
            assert!(
                matches!(parsed, Err(RoleError::Yaml { .. })),
                "malformed role block parsed instead of refusing: {yaml:?} gave {parsed:?}"
            );
        }
    }

    /// The test above does not test `deny_unknown_fields`, and I only found
    /// that out by deleting the attribute and watching all four of its cases
    /// stay green. Every one of them fails on `tool_groups` being REQUIRED —
    /// including the `toolgroups:` typo, which errors on the missing
    /// `tool_groups` rather than on the unknown one.
    ///
    /// These cases keep `tool_groups` valid and misspell one of the four
    /// OPTIONAL fields, which is the only shape the attribute is load-bearing
    /// for. Without it `personaldata: deny` parses as
    /// `RolePersonalData::Inherit` — the child silently keeping the parent's
    /// entire personal scope, on exactly the input a human got wrong. That is
    /// the widening default this whole module exists to refuse.
    #[test]
    fn a_typo_on_an_optional_field_refuses_rather_than_silently_defaulting() {
        for (typo, field, consequence) in [
            (
                "personaldata: deny",
                "personal_data",
                "the child keeps the parent's entire personal scope",
            ),
            (
                "personal-data: deny",
                "personal_data",
                "the child keeps the parent's entire personal scope",
            ),
            (
                "max_turn: 4",
                "max_turns",
                "the child gets the default turn budget instead of the one written down",
            ),
            (
                "contextfraction: 0.3",
                "context_fraction",
                "the child claims the default share of the parent's window",
            ),
            (
                "instruction: go",
                "instructions",
                "the child runs the recipe's prompt instead of the role's",
            ),
        ] {
            let yaml = format!("instructions: go\ngiap_role:\n  tool_groups: []\n  {typo}\n");
            let parsed = AgentRole::from_recipe_yaml("r", &yaml);
            assert!(
                matches!(parsed, Err(RoleError::Yaml { .. })),
                "`{typo}` was accepted as a role block, so `{field}` is unset and defaults: \
                 {consequence}. Got {parsed:?}"
            );
        }
    }

    /// Vacuity control for the test above: the correctly-spelled fields parse
    /// AND take effect, so the refusals next door are about the spelling rather
    /// than about the optional fields being unreadable in general.
    #[test]
    fn the_correctly_spelled_optional_fields_parse_and_take_effect() {
        let yaml = "instructions: from the recipe\n\
                    giap_role:\n  \
                      tool_groups: []\n  \
                      personal_data: deny\n  \
                      max_turns: 4\n  \
                      context_fraction: 0.3\n  \
                      instructions: from the block\n";
        let role = AgentRole::from_recipe_yaml("r", yaml).unwrap().unwrap();
        assert_eq!(role.personal_data(), RolePersonalData::Deny);
        assert_eq!(role.max_turns(), 4);
        assert!((role.context_fraction() - 0.3).abs() < f32::EPSILON);
        assert_eq!(role.instructions(), "from the block");
    }

    /// Vacuity control for the test above: the well-formed sibling of those
    /// cases must parse, or the assertions are only proving that YAML is hard.
    #[test]
    fn the_well_formed_sibling_of_the_malformed_cases_parses() {
        let yaml = "instructions: go\ngiap_role:\n  tool_groups:\n    - giap-weather\n";
        let role = AgentRole::from_recipe_yaml("r", yaml).unwrap().unwrap();
        assert_eq!(
            role.requested_tool_groups(),
            &["giap-weather".to_string()].into_iter().collect()
        );
        // Unstated limits fall to the documented defaults, not to Goose's 25.
        assert_eq!(role.max_turns(), DEFAULT_ROLE_MAX_TURNS);
        assert_eq!(role.personal_data(), RolePersonalData::Inherit);
    }

    /// An explicitly empty list is a legitimate, narrow choice -- a subagent
    /// that only reasons. It must be distinguishable from an absent key.
    #[test]
    fn an_explicitly_empty_group_list_is_allowed_and_stays_empty() {
        let yaml = "instructions: go\ngiap_role:\n  tool_groups: []\n";
        let role = AgentRole::from_recipe_yaml("r", yaml).unwrap().unwrap();
        assert!(role.requested_tool_groups().is_empty());
    }

    /// The turn budget and its cap, pinned to the literals the way
    /// `depth_tests::the_depth_cap_is_one` pins its own.
    ///
    /// Every other assertion about these two numbers is self-referential —
    /// `MAX_ROLE_MAX_TURNS + 1` for the refusal, `== DEFAULT_ROLE_MAX_TURNS`
    /// for the default — so they follow the constants wherever the constants
    /// go. 6 -> 25 and 12 -> 100 together left the whole file green, and the
    /// comment next door claiming the default is "not Goose's 25" could not
    /// see 25.
    #[test]
    fn the_role_turn_budget_and_its_cap_are_the_numbers_the_design_chose() {
        assert_eq!(
            DEFAULT_ROLE_MAX_TURNS, 6,
            "the unstated turn budget moved. Goose's own default is 25, which is minutes of \
             wall clock on an Orin with the parent's turn blocked behind it — if this is now \
             25, say why in the constant's doc"
        );
        assert_eq!(
            MAX_ROLE_MAX_TURNS, 12,
            "the largest budget a stored role may ask for moved. It is a refusal rather than a \
             clamp, so raising it is a decision about how long a subagent may hold the GPU"
        );
        assert!(
            DEFAULT_ROLE_MAX_TURNS <= MAX_ROLE_MAX_TURNS,
            "the default turn budget is above its own cap, so every role that states nothing \
             is refused"
        );
    }

    /// The default share of the parent's window, pinned to its literal and
    /// wired to the behaviour that reads it — nothing read
    /// `DEFAULT_CONTEXT_FRACTION` at all, so 0.5 -> 1.0 was green, and 1.0 is a
    /// role claiming the parent's WHOLE history budget.
    #[test]
    fn an_unstated_context_fraction_is_half_the_parents_budget() {
        assert!(
            (DEFAULT_CONTEXT_FRACTION - 0.5).abs() < f32::EPSILON,
            "the default context fraction moved to {DEFAULT_CONTEXT_FRACTION}; at 1.0 a role \
             that states nothing claims the parent's entire history budget"
        );
        let role =
            AgentRole::from_recipe_yaml("r", "instructions: go\ngiap_role:\n  tool_groups: []\n")
                .unwrap()
                .unwrap();
        assert!(
            (role.context_fraction() - DEFAULT_CONTEXT_FRACTION).abs() < f32::EPSILON,
            "a role that states no context_fraction did not get the documented default"
        );
    }

    #[test]
    fn a_role_asking_for_more_turns_than_the_cap_is_refused() {
        let yaml = format!(
            "instructions: go\ngiap_role:\n  tool_groups: []\n  max_turns: {}\n",
            MAX_ROLE_MAX_TURNS + 1
        );
        assert!(matches!(
            AgentRole::from_recipe_yaml("r", &yaml),
            Err(RoleError::MaxTurnsOutOfRange { .. })
        ));
        assert!(matches!(
            AgentRole::from_recipe_yaml(
                "r",
                "instructions: go\ngiap_role:\n  tool_groups: []\n  max_turns: 0\n"
            ),
            Err(RoleError::MaxTurnsOutOfRange { .. })
        ));
    }

    #[test]
    fn a_context_fraction_outside_the_unit_interval_is_refused() {
        for bad in ["0.0", "1.5", "-0.2"] {
            let yaml = format!(
                "instructions: go\ngiap_role:\n  tool_groups: []\n  context_fraction: {bad}\n"
            );
            assert!(
                matches!(
                    AgentRole::from_recipe_yaml("r", &yaml),
                    Err(RoleError::ContextFractionOutOfRange { .. })
                ),
                "context_fraction {bad} was accepted"
            );
        }
    }

    #[test]
    fn a_role_with_no_instructions_anywhere_is_refused() {
        let yaml = "title: t\ngiap_role:\n  tool_groups: []\n";
        assert!(matches!(
            AgentRole::from_recipe_yaml("r", yaml),
            Err(RoleError::MissingInstructions { .. })
        ));
    }

    /// The role block rides a recipe that `pond-api`'s `RecipePrompt` and
    /// Goose's own `Recipe` both still have to be able to read, which is the
    /// whole basis of "no new persistence". Neither sets
    /// `deny_unknown_fields`, so this asserts the shape they see: a document
    /// whose other keys are untouched and whose prompt is still there.
    #[test]
    fn the_role_block_leaves_the_rest_of_the_recipe_readable() {
        let doc: serde_yaml::Value = serde_yaml::from_str(ROLE_YAML).unwrap();
        assert_eq!(
            doc.get("title").and_then(|v| v.as_str()),
            Some("Researcher")
        );
        assert!(doc.get(ROLE_YAML_KEY).is_some());
        assert!(doc.get("instructions").and_then(|v| v.as_str()).is_some());
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;

    fn run(status: TaskStatus) -> TaskRun {
        TaskRun {
            id: "t1".into(),
            role: "r".into(),
            parent_session_id: "s1".into(),
            status,
            result: Some("here is the answer".into()),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
        }
    }

    /// The engine returns `Ok(text)` for a cancelled child and `Ok(budget
    /// message)` for one that ran out of turns. Neither is an answer, and the
    /// parent must not be told it is.
    #[test]
    fn only_a_completed_run_yields_a_result_to_the_parent() {
        assert_eq!(
            run(TaskStatus::Completed).result_for_parent(),
            Some("here is the answer")
        );
        for status in [
            TaskStatus::Queued,
            TaskStatus::Running,
            TaskStatus::Cancelled,
            TaskStatus::TurnBudgetExhausted,
            TaskStatus::Failed,
        ] {
            assert_eq!(
                run(status).result_for_parent(),
                None,
                "{status:?} was allowed to report a result to the parent"
            );
        }
    }

    #[test]
    fn terminal_states_are_the_four_that_stop() {
        assert!(!TaskStatus::Queued.is_terminal());
        assert!(!TaskStatus::Running.is_terminal());
        for status in [
            TaskStatus::Completed,
            TaskStatus::Cancelled,
            TaskStatus::TurnBudgetExhausted,
            TaskStatus::Failed,
        ] {
            assert!(status.is_terminal(), "{status:?} should be terminal");
        }
    }

    #[test]
    fn a_run_starts_from_its_spec_and_carries_no_transcript() {
        let parent = DelegationAuthority::root(
            "s1",
            ProfileScope::Household,
            ["giap-weather".to_string()].into_iter().collect(),
        );
        let role = AgentRole::new(
            "r",
            "go",
            ["giap-weather".to_string()].into_iter().collect(),
            RolePersonalData::Inherit,
            3,
            0.5,
        )
        .unwrap();
        let spec = parent
            .delegate(
                &role,
                TaskRequest {
                    role: "r".into(),
                    instructions: "go".into(),
                    inputs: serde_json::Value::Null,
                },
            )
            .unwrap();
        let started = TaskRun::started(&spec, Utc::now());
        assert_eq!(started.id, spec.id());
        assert_eq!(started.parent_session_id, "s1");
        assert_eq!(started.status, TaskStatus::Running);
        assert_eq!(started.result_for_parent(), None);
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use crate::models::services::context::model_class::ON_DEVICE_PROVIDERS;

    /// The trap this test exists for: invariant 3 is WORDED as "local/gguf",
    /// and pinning only those two leaves `ollama` and `llamafile` -- which on a
    /// pond talk to 127.0.0.1 and contend for the same GPU -- free to run
    /// subagents in parallel. Iterating the shared constant means a provider
    /// added to it later is covered without anyone remembering this file.
    #[test]
    fn nothing_that_runs_on_this_device_gets_parallelism() {
        for provider in ON_DEVICE_PROVIDERS {
            assert_eq!(
                max_concurrent_subagents(provider),
                1,
                "{provider} runs on this box and must not run subagents in parallel"
            );
        }
        // The two the narrow reading of the invariant would have missed.
        assert_eq!(max_concurrent_subagents("ollama"), 1);
        assert_eq!(max_concurrent_subagents("llamafile"), 1);
    }

    /// Vacuity control: if the function returned 1 for everything, the test
    /// above would pass while the feature did nothing for hosted providers.
    #[test]
    fn a_hosted_provider_is_allowed_more_than_one() {
        assert!(max_concurrent_subagents("anthropic") > 1);
        assert_eq!(
            max_concurrent_subagents("openai"),
            REMOTE_SUBAGENT_CONCURRENCY
        );
    }

    /// The remote number, pinned to its literal. The test above asserts only
    /// `> 1` and an equality against the constant itself, so 3 -> 64 was green
    /// — and an unmeasured constant is exactly the kind that drifts, because
    /// there is no measurement to contradict it.
    #[test]
    fn the_remote_concurrency_number_is_the_one_that_was_written_down() {
        assert_eq!(
            REMOTE_SUBAGENT_CONCURRENCY, 3,
            "the hosted-provider concurrency moved. Nothing has measured it, so a change here \
             is a guess replacing a guess and belongs in the constant's doc"
        );
    }

    #[test]
    fn the_predicate_is_case_insensitive_like_the_one_it_delegates_to() {
        assert_eq!(max_concurrent_subagents("Ollama"), 1);
        assert_eq!(max_concurrent_subagents("GGUF"), 1);
    }
}
