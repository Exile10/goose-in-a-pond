//! PAI-6 P2 guards.
//!
//! The engine drive itself (`GooseAdapter::run_child_agent`) is not covered
//! here and cannot be: it needs a real provider and a real Goose session store.
//! What IS covered is everything that decides — the plan, the concurrency
//! permit, the outcome classification, the registry and the invariant-2 audit —
//! driven through the real [`GooseOrchestrator::spawn`] against a fake
//! [`ChildRunner`]. The `ChildPlan` the fake receives is produced by production
//! code, so the assertions are about the artifact that actually reaches the
//! engine.

use super::*;
use pond_core::models::services::context::model_class::ON_DEVICE_PROVIDERS;
use pond_core::shared::domain::orchestration::{
    AgentRole, DelegationAuthority, RolePersonalData, TaskRequest,
};
use pond_core::shared::services::turn_authority::TurnAuthorityLease;
use pond_core::user_data::domain::profile::ProfileScope;
use std::sync::atomic::{AtomicUsize, Ordering};

// ── Fixtures ────────────────────────────────────────────────────────────────

fn role(name: &str, groups: &[&str]) -> AgentRole {
    AgentRole::new(
        name,
        "Answer the question from the tools you have, then stop.",
        groups.iter().map(|g| g.to_string()).collect(),
        RolePersonalData::Inherit,
        4,
        0.3,
    )
    .expect("fixture role is valid")
}

/// Every `TaskSpec` in this file is built the only way production can build one
/// — through a real `DelegationAuthority`. `TaskSpec` has private fields, no
/// constructor and no `Deserialize` precisely so that a fixture cannot invent
/// an authority the parent never had.
fn spec_for(role: &AgentRole, parent_groups: &[&str]) -> TaskSpec {
    DelegationAuthority::root(
        "parent-session",
        ProfileScope::Household,
        parent_groups.iter().map(|g| g.to_string()).collect(),
    )
    .delegate(
        role,
        TaskRequest {
            role: role.name().to_string(),
            instructions: "what is the weather".to_string(),
            inputs: serde_json::Value::Null,
        },
    )
    .expect("fixture delegation is authorised")
}

/// The GIAP session id every fixture spec names as its parent.
const PARENT_SESSION: &str = "parent-session";

/// An orchestrator whose parent turn is LIVE, published the way
/// `GooseAdapter::chat_stream` publishes one.
///
/// PAI-6 P3 made this mandatory rather than incidental: `spawn` refuses a spec
/// whose parent turn has ended, because the authority behind it is then stale.
/// The lease has to be held by the caller for exactly as long as the parent's
/// stream closure holds its own — which is why it comes back rather than being
/// dropped here.
fn live_turn(runner: Arc<dyn ChildRunner>) -> (Arc<GooseOrchestrator>, TurnAuthorityLease) {
    let (orchestrator, lease, _token) = live_turn_with_token(runner);
    (orchestrator, lease)
}

fn live_turn_with_token(
    runner: Arc<dyn ChildRunner>,
) -> (
    Arc<GooseOrchestrator>,
    TurnAuthorityLease,
    CancellationToken,
) {
    let authorities = Arc::new(TurnAuthorityRegistry::new());
    let token = CancellationToken::new();
    let lease = authorities.publish(
        "parent-goose-session",
        DelegationAuthority::root(
            PARENT_SESSION,
            ProfileScope::Household,
            ["giap-weather".to_string()].into_iter().collect(),
        ),
        token.clone(),
    );
    (
        Arc::new(GooseOrchestrator::new(runner, authorities)),
        lease,
        token,
    )
}

fn parent_tools(entries: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
    entries
        .iter()
        .map(|(ext, tools)| {
            (
                (*ext).to_string(),
                tools.iter().map(|t| (*t).to_string()).collect(),
            )
        })
        .collect()
}

fn env_with(provider: &str, tools: BTreeMap<String, BTreeSet<String>>) -> ChildEnvironment {
    ChildEnvironment {
        provider_name: provider.to_string(),
        base_system_prefix: "You are Pond, a privacy-first local assistant.".to_string(),
        parent_tools: tools,
    }
}

fn granted_tools(config: &ExtensionConfig) -> &[String] {
    match config {
        ExtensionConfig::Builtin {
            available_tools, ..
        } => available_tools,
        other => panic!("expected a Builtin extension config, got {other:?}"),
    }
}

fn extension_name(config: &ExtensionConfig) -> String {
    config.name().to_string()
}

// ── The fake engine ─────────────────────────────────────────────────────────

#[derive(Default)]
struct FakeRunnerState {
    plans: Mutex<Vec<ChildPlan>>,
    /// Child engine sessions this fake was asked to create. PAI-6 P3 refuses
    /// some delegations before the engine is touched at all, and "refused" has
    /// to be distinguishable from "ran and failed".
    opened: Mutex<Vec<String>>,
    released: Mutex<Vec<String>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    session_seq: AtomicUsize,
}

struct FakeRunner {
    state: Arc<FakeRunnerState>,
    env: ChildEnvironment,
    /// What the fake engine "returns". `None` means it panics if run — used by
    /// tests that expect the plan builder to refuse before anything runs.
    outcome: Option<ChildOutcome>,
    /// Cancel the run's own token from inside the run, so the post-await
    /// re-check has something to see. This is the shape Goose actually
    /// produces: the reply loop breaks and returns `Ok(partial_text)`.
    cancel_from_inside: bool,
    /// Milliseconds to hold the concurrency permit, so overlap is observable.
    hold_ms: u64,
}

impl FakeRunner {
    fn new(env: ChildEnvironment) -> Self {
        Self {
            state: Arc::new(FakeRunnerState::default()),
            env,
            outcome: Some(ChildOutcome {
                last_text: Some("the weather is fine".to_string()),
                assistant_turns: 1,
                loaded_extensions: BTreeSet::new(),
            }),
            cancel_from_inside: false,
            hold_ms: 0,
        }
    }

    fn returning(mut self, outcome: ChildOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    fn cancelling_itself(mut self) -> Self {
        self.cancel_from_inside = true;
        self
    }

    fn holding_for(mut self, ms: u64) -> Self {
        self.hold_ms = ms;
        self
    }
}

#[async_trait]
impl ChildRunner for FakeRunner {
    async fn environment(&self, _parent_session_id: &str) -> Result<ChildEnvironment> {
        Ok(self.env.clone())
    }

    async fn open_child_session(&self, plan_role: &str) -> Result<String> {
        let n = self.state.session_seq.fetch_add(1, Ordering::SeqCst);
        let id = format!("child-{plan_role}-{n}");
        self.state
            .opened
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(id.clone());
        Ok(id)
    }

    async fn run(&self, plan: ChildPlan, cancel: CancellationToken) -> Result<ChildOutcome> {
        let now = self.state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.state
            .plans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(plan);
        if self.hold_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.hold_ms)).await;
        }
        if self.cancel_from_inside {
            cancel.cancel();
        }
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(self
            .outcome
            .clone()
            .expect("this fake was not meant to be run"))
    }

    async fn release(&self, child_session_id: &str) {
        self.state
            .released
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(child_session_id.to_string());
    }
}

// ── Invariant 3: concurrency 1 on anything that runs on this device ─────────

/// The predicate, pinned against `ON_DEVICE_PROVIDERS` rather than against the
/// two names PAI-6's invariant text used to say. A test written as
/// `subagent_permits("local") == 3` would be a vacuity control pinning the
/// wrong number: it stays green while `ollama` — the same GPU, over
/// `127.0.0.1` — quietly gets three concurrent children.
#[test]
fn every_provider_that_runs_on_this_device_takes_the_whole_semaphore() {
    for provider in ON_DEVICE_PROVIDERS {
        assert_eq!(
            subagent_permits(provider),
            SUBAGENT_PERMITS as u32,
            "provider `{provider}` runs on this device, so one child of it must hold every \
             permit - anything less lets two subagents share one GPU and one retained KV prefix"
        );
    }
}

/// The other half, so the test above cannot pass by `subagent_permits`
/// returning the whole semaphore for everything.
#[test]
fn a_provider_somewhere_else_takes_only_one_permit() {
    assert_eq!(subagent_permits("anthropic"), 1);
    assert!(
        subagent_permits("local") > subagent_permits("anthropic"),
        "the on-device and remote cases ask for the same number of permits, so the assertion \
         next door is satisfied by everything and proves nothing"
    );
}

#[tokio::test]
async fn only_one_on_device_child_runs_at_a_time() {
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(60));
    let state = runner.state.clone();
    let (orchestrator, _turn) = live_turn(runner);

    let mut handles = Vec::new();
    for _ in 0..3 {
        let orchestrator = orchestrator.clone();
        let spec = spec_for(&role, &["giap-weather"]);
        handles.push(tokio::spawn(async move { orchestrator.spawn(spec).await }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert_eq!(
        state.max_in_flight.load(Ordering::SeqCst),
        1,
        "three subagents ran concurrently on an on-device provider - they interleave on one \
         GPU and overwrite each other's retained KV prefix, so the parent pays a full \
         re-prefill on its next turn"
    );
}

/// Vacuity control for the test above: the fake CAN observe overlap, so
/// `max_in_flight == 1` is a real measurement rather than an artefact of the
/// harness never running two things at once.
#[tokio::test]
async fn the_overlap_detector_can_actually_see_overlap() {
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(60));
    let state = runner.state.clone();
    let (orchestrator, _turn) = live_turn(runner);

    let mut handles = Vec::new();
    for _ in 0..3 {
        let orchestrator = orchestrator.clone();
        let spec = spec_for(&role, &["giap-weather"]);
        handles.push(tokio::spawn(async move { orchestrator.spawn(spec).await }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert!(
        state.max_in_flight.load(Ordering::SeqCst) > 1,
        "a remote provider is allowed concurrency and the harness saw none, so the on-device \
         assertion next door proves nothing"
    );
}

// ── Invariant 1: a child's tools are a subset of its parent's ───────────────

#[test]
fn a_child_gets_only_what_its_spec_and_its_parent_both_have() {
    let role = role("researcher", &["giap-weather", "giap-knowledge"]);
    // The parent's authority does not include giap-knowledge, so P1's
    // intersection already dropped it; and the parent's engine never loaded
    // giap-news, so it cannot appear either.
    let spec = spec_for(&role, &["giap-weather", "giap-news"]);
    let tools = parent_tools(&[
        ("giap-weather", &["get_weather", "get_forecast"]),
        ("giap-knowledge", &["search"]),
        ("giap-news", &["headlines"]),
    ]);

    let configs = child_extensions(&spec, &tools).expect("nothing forbidden here");
    let names: Vec<String> = configs.iter().map(extension_name).collect();
    assert_eq!(
        names,
        vec!["giap-weather".to_string()],
        "the child was given an extension its spec did not authorise or its parent did not have \
         loaded - PAI-6 invariant 1 is that a subagent's scope is a SUBSET of its parent's"
    );
    // Sorted: the inventory is a BTreeSet, so the child's allowlist is stable
    // between runs and does not churn the tools JSON (and therefore the KV
    // prefix) for a reason nobody chose.
    assert_eq!(
        granted_tools(&configs[0]),
        ["get_forecast".to_string(), "get_weather".to_string()]
    );
}

/// The anti-`vec![]` guard, and the reason it exists is one line of Goose:
/// `available_tools.is_empty() || available_tools.contains(&tool_name)`.
#[test]
fn an_extension_is_never_emitted_with_an_empty_tool_allowlist() {
    let role = role("researcher", &["giap-weather", "giap-knowledge"]);
    let spec = spec_for(&role, &["giap-weather", "giap-knowledge"]);
    // giap-knowledge is authorised, loaded on the parent, and has no tools.
    let tools = parent_tools(&[("giap-weather", &["get_weather"]), ("giap-knowledge", &[])]);

    let configs = child_extensions(&spec, &tools).unwrap();
    for config in &configs {
        assert!(
            !granted_tools(config).is_empty(),
            "extension `{}` was emitted with an empty available_tools, which Goose reads as \
             ALL TOOLS of that extension - the scope-widening default this phase exists to \
             avoid",
            extension_name(config)
        );
    }
    assert_eq!(
        configs.len(),
        1,
        "the empty extension must be dropped, not emitted"
    );
}

/// Goose matches `available_tools` against the UNPREFIXED name, inside
/// `dispatch_tool_call`. Emitting `giap-weather__get_weather` there would
/// silently deny every tool of the extension at dispatch time while the model
/// still saw them listed.
#[test]
fn available_tools_are_unprefixed_because_that_is_what_goose_matches() {
    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    let configs =
        child_extensions(&spec, &parent_tools(&[("giap-weather", &["get_weather"])])).unwrap();
    assert_eq!(granted_tools(&configs[0]), ["get_weather".to_string()]);
    assert!(
        !granted_tools(&configs[0])
            .iter()
            .any(|t| t.contains(TOOL_NAME_SEPARATOR)),
        "available_tools carried a prefixed name; ExtensionConfig::is_tool_available is called \
         with resolved.actual_tool_name, so every tool would be refused at dispatch"
    );
}

// ── Invariant 2: the ten stripped builtins stay stripped ───────────────────

#[test]
fn the_stripped_builtins_are_still_these_ten() {
    assert_eq!(
        GOOSE_STRIPPED_BUILTINS,
        [
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
        ]
    );
}

/// A producible fixture, not a hypothetical one: the parent's strip is a
/// best-effort `remove_extension(...).ok()`, so a parent whose strip failed
/// really does list `developer__shell` in `list_tools`.
///
/// What does the work here is `TaskSpec::grants_tool`, not a second
/// stripped-builtin check — I wrote one, found that deleting it left this test
/// green because a name the spec does not grant is dropped anyway, and removed
/// it rather than keep a guard something else was silently satisfying.
/// Defeating `grants_tool` is what makes this test fail.
#[test]
fn a_parent_whose_strip_failed_does_not_hand_its_builtins_to_a_child() {
    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    let tools = parent_tools(&[
        ("giap-weather", &["get_weather"]),
        ("developer", &["shell"]),
        ("summon", &["delegate"]),
    ]);

    let configs = child_extensions(&spec, &tools).unwrap();
    let names: Vec<String> = configs.iter().map(extension_name).collect();
    for stripped in GOOSE_STRIPPED_BUILTINS {
        assert!(
            !names.iter().any(|n| n == stripped),
            "the child was handed `{stripped}`, one of the ten builtins GIAP strips from every \
             session - `summon` and `orchestrator` are how a subagent spawns a subagent"
        );
    }
}

/// The other direction: an authority that somehow names a stripped builtin is a
/// refusal to run, not a silent drop. Synthetic by construction — three
/// upstream narrowings would each have to be weakened for it to arrive — which
/// is exactly why it is here: it is the one that survives them.
#[test]
fn a_spec_naming_a_stripped_builtin_refuses_to_produce_a_plan() {
    let role = role("saboteur", &["summon"]);
    let spec = spec_for(&role, &["summon"]);
    let refused = child_extensions(&spec, &parent_tools(&[("summon", &["delegate"])]))
        .expect_err("a spec naming `summon` must refuse");
    assert!(
        matches!(
            &refused,
            PlanRefused::ForbiddenExtension { extension, .. } if extension == "summon"
        ),
        "expected a ForbiddenExtension refusal naming summon, got {refused:?}"
    );
}

#[tokio::test]
async fn a_child_that_ended_up_with_a_stripped_builtin_has_its_result_discarded() {
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(
        FakeRunner::new(env).returning(ChildOutcome {
            last_text: Some("I ran a shell command for you".to_string()),
            assistant_turns: 1,
            loaded_extensions: ["giap-weather".to_string(), "summon".to_string()]
                .into_iter()
                .collect(),
        }),
    );
    let (orchestrator, _turn) = live_turn(runner);

    let run = orchestrator
        .spawn(spec_for(&role, &["giap-weather"]))
        .await
        .unwrap();
    assert_eq!(
        run.status,
        TaskStatus::Failed,
        "a child that loaded a stripped builtin was reported as a success"
    );
    assert_eq!(
        run.result_for_parent(),
        None,
        "the answer of a child that loaded `summon` reached the parent"
    );
}

/// The canary the phase asked for, in the spirit of the fork-list canaries
/// elsewhere: if a future refactor puts these names back into `goose_agent.rs`
/// by hand, the two copies can drift again and only one of them is the guard.
#[test]
fn goose_agent_reads_the_one_stripped_builtin_list_rather_than_its_own() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/goose_agent.rs"))
            .expect("goose_agent.rs is next door");
    let code = strip_line_comments(&source);

    assert!(
        code.matches("GOOSE_STRIPPED_BUILTINS").count() >= 2,
        "goose_agent.rs must read the shared list in BOTH places that name these builtins - \
         the strip guard and the prompt filter"
    );
    for stripped in GOOSE_STRIPPED_BUILTINS {
        assert!(
            !code.contains(&format!("\"{stripped}\"")),
            "goose_agent.rs hardcodes the builtin name `{stripped}` again outside \
             GOOSE_STRIPPED_BUILTINS; two copies of this list is how one of them drifts while \
             the other is the only one that enforces anything"
        );
    }
}

/// Comments mention these names constantly and a guard satisfied by comment
/// prose is one of this programme's recorded vacuity shapes.
fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            // Good enough here: this file has no `"` containing `//` on a line
            // that also declares an extension name, and the test asserts an
            // ABSENCE, so an over-eager strip can only make it laxer in a way
            // the positive assertion above still catches.
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The sentinel Goose returns instead of an error when a child runs out of
/// turns. It is a private `const` over there, so this is a mirror, and a mirror
/// that has silently stopped matching is worse than no mirror.
#[test]
fn the_goose_turn_cap_message_is_still_verbatim() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../goose/crates/goose/src/agents/agent.rs"
    ))
    .expect("the goose submodule must be initialised - `git submodule update --init --recursive`");
    let expected = format!("const MAX_TURNS_MESSAGE: &str = \"{GOOSE_MAX_TURNS_MESSAGE}\";");
    assert!(
        source.contains(&expected),
        "goose's MAX_TURNS_MESSAGE no longer matches the string this adapter checks for, so an \
         exhausted turn budget will be handed to the user as the subagent's answer"
    );
}

// ── Cancellation and budget exhaustion both return Ok from Goose ────────────

#[test]
fn a_cancelled_run_is_not_an_answer_however_much_text_came_back() {
    let (status, result) = classify_outcome(true, 2, 4, Some("here is half an answer"));
    assert_eq!(status, TaskStatus::Cancelled);
    assert_eq!(result, None);
}

#[test]
fn an_exhausted_turn_budget_is_not_an_answer() {
    let (by_sentinel, sentinel_result) =
        classify_outcome(false, 3, 4, Some(GOOSE_MAX_TURNS_MESSAGE));
    assert_eq!(
        by_sentinel,
        TaskStatus::TurnBudgetExhausted,
        "goose's own turn-cap sentence was taken for the subagent's answer - goose returns Ok \
         with MAX_TURNS_MESSAGE as the text when a child runs out of turns"
    );
    assert_eq!(
        sentinel_result, None,
        "the turn-cap sentence would have been reported to the user as the delegated result"
    );

    // Goose trips at `turns_taken > max_turns`, so crossing the count is the
    // same event seen from the other side.
    let (by_count, count_result) = classify_outcome(false, 5, 4, Some("still thinking"));
    assert_eq!(
        by_count,
        TaskStatus::TurnBudgetExhausted,
        "a child that took more turns than its budget was reported as having finished"
    );
    assert_eq!(count_result, None);
}

/// Vacuity control for the two above: an ordinary run DOES produce an answer,
/// so `result == None` is a decision rather than the only thing this function
/// can return.
#[test]
fn an_ordinary_run_produces_an_answer() {
    let (status, result) = classify_outcome(false, 2, 4, Some("  the weather is fine  "));
    assert_eq!(status, TaskStatus::Completed);
    assert_eq!(result.as_deref(), Some("the weather is fine"));
    assert!(status.produced_an_answer());
}

#[test]
fn a_run_that_said_nothing_is_a_failure_rather_than_an_empty_answer() {
    assert_eq!(classify_outcome(false, 1, 4, None).0, TaskStatus::Failed);
    assert_eq!(
        classify_outcome(false, 1, 4, Some("   ")).0,
        TaskStatus::Failed
    );
}

/// The end-to-end shape, because the classification above only matters if
/// `spawn` re-checks the token AFTER the await. Goose's reply loop breaks out
/// and returns `Ok(partial_text)`, so the `Result` cannot say.
#[tokio::test]
async fn a_child_cancelled_mid_run_reports_cancelled_not_completed() {
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(
        FakeRunner::new(env)
            .cancelling_itself()
            .returning(ChildOutcome {
                last_text: Some("half an answer".to_string()),
                assistant_turns: 1,
                loaded_extensions: BTreeSet::new(),
            }),
    );
    let (orchestrator, _turn) = live_turn(runner);

    let run = orchestrator
        .spawn(spec_for(&role, &["giap-weather"]))
        .await
        .unwrap();
    assert_eq!(
        run.status,
        TaskStatus::Cancelled,
        "the engine returned Ok with text after cancellation and it was taken at face value"
    );
    assert_eq!(run.result_for_parent(), None);
}

#[tokio::test]
async fn cancelling_a_parent_cancels_the_child_that_is_still_running() {
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(120));
    let (orchestrator, _turn) = live_turn(runner);

    let spawner = {
        let orchestrator = orchestrator.clone();
        let spec = spec_for(&role, &["giap-weather"]);
        tokio::spawn(async move { orchestrator.spawn(spec).await })
    };
    // Let the run reach the engine before cancelling the parent.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let stopped = orchestrator
        .cancel_children_of("parent-session")
        .await
        .unwrap();
    assert_eq!(stopped, 1, "cancelling the parent stopped no children");

    let run = spawner.await.unwrap().unwrap();
    assert_eq!(
        run.status,
        TaskStatus::Cancelled,
        "the parent was cancelled and its child still reported an answer - PAI-6 invariant 5 is \
         that cancelling a parent cancels its children"
    );
    assert_eq!(run.result_for_parent(), None);
}

#[tokio::test]
async fn cancelling_a_different_parent_leaves_this_ones_children_alone() {
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(120));
    let (orchestrator, _turn) = live_turn(runner);

    let spawner = {
        let orchestrator = orchestrator.clone();
        let spec = spec_for(&role, &["giap-weather"]);
        tokio::spawn(async move { orchestrator.spawn(spec).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(
        orchestrator
            .cancel_children_of("somebody-elses-session")
            .await
            .unwrap(),
        0,
        "cancel_children_of stopped a run belonging to a different parent session - one user \
         ending their turn would kill another household member's delegation"
    );

    let run = spawner.await.unwrap().unwrap();
    assert_eq!(
        run.status,
        TaskStatus::Completed,
        "cancelling one parent's children stopped another parent's"
    );
}

// ── The prompt: THE respecification of this phase ───────────────────────────

#[test]
fn the_child_prompt_is_giaps_own_and_states_the_limits_the_child_runs_under() {
    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let plan = build_child_plan(&spec, "child-1", &env).unwrap();

    assert!(
        plan.system_prompt.starts_with(&env.base_system_prefix),
        "the child's prompt must start from the SAME `build_prompt_partition` prefix the parent \
         uses; the phase bullet said to render `subagent_system.md`, which is deleted"
    );
    assert!(
        plan.system_prompt.contains("researcher"),
        "the role is not named"
    );
    assert!(
        plan.system_prompt.contains("at most 4 turns"),
        "the child is not told its turn budget"
    );
    assert!(
        plan.system_prompt.contains("cannot delegate"),
        "the child is not told it is at the depth cap"
    );
    assert!(
        plan.system_prompt.contains("giap-weather__get_weather"),
        "the child is not told which tools it has"
    );
    for leak in ["goose AI framework", "AAIF", "Agentic AI Foundation"] {
        assert!(
            !plan.system_prompt.contains(leak),
            "the child's prompt carries `{leak}` - that is Goose's own subagent_system.md, which \
             GiapProviderShim does not veto because its GOOSE_DEFAULT_MARKER is not in it"
        );
    }
    assert_eq!(plan.max_turns, 4);
    assert_eq!(plan.user_message, "what is the weather");
}

#[test]
fn a_child_with_no_tools_is_told_so_rather_than_shown_an_empty_list() {
    let role = role("summariser", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    // Authorised for giap-weather, but the parent has nothing loaded.
    let env = env_with("ollama", parent_tools(&[]));
    let plan = build_child_plan(&spec, "child-1", &env).unwrap();
    assert!(plan.extensions.is_empty());
    assert!(plan.system_prompt.contains("no tools on this run"));
}

// ── The registry ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_finished_run_can_be_polled_and_listed_and_an_unknown_one_cannot() {
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env));
    let state = runner.state.clone();
    let (orchestrator, _turn) = live_turn(runner);

    let run = orchestrator
        .spawn(spec_for(&role, &["giap-weather"]))
        .await
        .unwrap();

    let polled = orchestrator.poll(&run.id).await.unwrap().expect("known id");
    assert_eq!(polled.status, TaskStatus::Completed);
    assert_eq!(polled.result_for_parent(), Some("the weather is fine"));
    assert!(orchestrator.poll("no-such-task").await.unwrap().is_none());

    let listed = orchestrator.list("parent-session").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(orchestrator.list("other-session").await.unwrap().is_empty());

    assert_eq!(
        state.released.lock().unwrap().len(),
        1,
        "the child's engine session was not released - every delegation would leak a row into \
         goose's sessions.db that nothing in GIAP will ever read or clean up"
    );
}

#[test]
fn eviction_never_removes_a_run_that_is_still_going() {
    let role = role("researcher", &["giap-weather"]);
    let mut registry = TaskRegistry::default();
    let mut running_ids = Vec::new();

    for _ in 0..MAX_TRACKED_TASKS {
        let spec = spec_for(&role, &["giap-weather"]);
        let run = TaskRun::started(&spec, chrono::Utc::now());
        running_ids.push(run.id.clone());
        registry.insert(run, CancellationToken::new());
    }
    // One more than the cap, with nothing terminal to evict.
    let spec = spec_for(&role, &["giap-weather"]);
    let extra = TaskRun::started(&spec, chrono::Utc::now());
    let extra_id = extra.id.clone();
    registry.insert(extra, CancellationToken::new());

    for id in &running_ids {
        assert!(
            registry.tasks.contains_key(id),
            "a RUNNING task was evicted; it can no longer be cancelled or polled, which is the \
             failure this cap exists to bound rather than cause"
        );
    }
    assert!(registry.tasks.contains_key(&extra_id));

    // Now finish one and add another: the terminal one is what goes.
    registry.finish(&running_ids[0], TaskStatus::Completed, None, None);
    let spec = spec_for(&role, &["giap-weather"]);
    registry.insert(
        TaskRun::started(&spec, chrono::Utc::now()),
        CancellationToken::new(),
    );
    assert!(
        !registry.tasks.contains_key(&running_ids[0]),
        "eviction did not reclaim the finished run, so the registry grows without bound"
    );
}

// ── PAI-6 P3: scope inheritance at the edge ─────────────────────────────────

/// The refusal that makes the authority registry load-bearing rather than
/// decorative. A `TaskSpec` was authorised by a turn; once that turn has ended
/// the authority behind it is stale, and this programme's rule is that access
/// narrows on failure.
#[tokio::test]
async fn a_delegation_whose_parent_turn_has_ended_does_not_run() {
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env));
    let state = runner.state.clone();
    let (orchestrator, lease) = live_turn(runner);

    let spec = spec_for(&role, &["giap-weather"]);
    drop(lease);

    let refused = orchestrator
        .spawn(spec)
        .await
        .expect_err("a delegation ran after its parent turn had ended");
    assert!(
        refused.to_string().contains("parent-session"),
        "the refusal does not name the session whose turn is gone: {refused}"
    );
    assert!(
        state.plans.lock().unwrap().is_empty(),
        "the engine ran a delegation nobody live had authorised"
    );
    assert!(
        state.opened.lock().unwrap().is_empty(),
        "a child engine session was created for a delegation that was refused"
    );
}

/// Vacuity control for the test above: the SAME spec runs when the lease is
/// still held, so the refusal is about the turn having ended and not about the
/// fixture being unrunnable.
#[tokio::test]
async fn the_same_delegation_runs_while_its_parent_turn_is_live() {
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env));
    let (orchestrator, _turn) = live_turn(runner);
    let run = orchestrator
        .spawn(spec_for(&role, &["giap-weather"]))
        .await
        .expect("a live parent turn must be able to delegate");
    assert_eq!(run.status, TaskStatus::Completed);
}

/// PAI-6 invariant 5's other half, which P2 left open because the parent turn's
/// token was a stack local in a stream closure with no accessor. The child's
/// token is DERIVED from the parent's rather than minted fresh, so cancelling
/// the turn — a voice interrupt, a dropped stream, a client hanging up —
/// reaches the child without anything having to remember to call
/// `cancel_children_of`.
#[tokio::test]
async fn cancelling_the_parents_turn_cancels_a_child_derived_from_it() {
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(120));
    let (orchestrator, _turn, parent_token) = live_turn_with_token(runner);

    let spawner = {
        let orchestrator = orchestrator.clone();
        let spec = spec_for(&role, &["giap-weather"]);
        tokio::spawn(async move { orchestrator.spawn(spec).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    parent_token.cancel();

    let run = spawner.await.unwrap().unwrap();
    assert_eq!(
        run.status,
        TaskStatus::Cancelled,
        "the parent's TURN was cancelled and its child carried on - the child's token is not \
         derived from the parent's"
    );
    assert_eq!(run.result_for_parent(), None);
}

/// The child's shim allow-set and its `available_tools` must be the same
/// decision expressed twice, not two lists that can drift. `build_child_plan`
/// derives one from the other for exactly that reason.
#[test]
fn the_plans_allowed_tool_names_are_its_available_tools_and_nothing_else() {
    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather", "giap-memory"]);
    let env = env_with(
        "ollama",
        parent_tools(&[
            ("giap-weather", &["get_weather", "get_forecast"]),
            ("giap-memory", &["recall_memories"]),
        ]),
    );
    let plan = build_child_plan(&spec, "child-1", &env).unwrap();

    let from_extensions: BTreeSet<String> = plan
        .extensions
        .iter()
        .flat_map(|config| match config {
            ExtensionConfig::Builtin {
                name,
                available_tools,
                ..
            } => available_tools
                .iter()
                .map(|tool| format!("{name}__{tool}"))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    let published: BTreeSet<String> = plan.allowed_tool_names.iter().cloned().collect();
    assert_eq!(
        published, from_extensions,
        "the shim allow-set and available_tools disagree; one of the two layers of invariant \
         1 is guarding a different set from the other"
    );
    assert_eq!(
        published,
        ["giap-weather__get_weather", "giap-weather__get_forecast"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>(),
        "the role asked for giap-weather only and the plan published something else"
    );
    assert!(
        !published.contains("giap-memory__recall_memories"),
        "a tool the role never asked for was published to the child's shim entry"
    );
}

/// The "test the INPUT, not just the gate" guard.
///
/// `DelegationAuthority::delegate` intersects with whatever the root authority
/// holds, so every assertion in `pond-core` about narrowing is only worth
/// something if the root is built from the turn's REAL post-selection,
/// post-guest-subtraction allow-set. That construction lives inside
/// `chat_stream`, which needs a live engine, so this reads the source: it fails
/// if the publish is moved above the guest subtraction, if it stops using the
/// same `allowed_tools` binding that is published to the shim, or if the scope
/// stops coming from `turn_scope`.
///
/// PAI-1 P5 shipped inert for exactly the first of those reasons, and PAI-5 P1
/// leaked for the second: the gate was right and one of its inputs was not.
#[test]
fn the_turn_authority_is_built_from_the_published_allow_set() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/goose_agent.rs"))
            .expect("goose_agent.rs is next door");
    let code = strip_line_comments(&source);

    let subtract = code
        .find("subtract_guest_denied_tools")
        .expect("PAI-1 P5's guest subtraction is gone from the turn path");
    let publish_to_shim = code
        .find("set_allowed_tools(allowed_tools.clone())")
        .expect("the turn's allow-set is no longer published to the shim");
    let publish_authority = code
        .find("turn_authorities.publish")
        .expect("no turn publishes a delegation authority, so PAI-6 P3 is inert");

    assert!(
        publish_authority > subtract,
        "the delegation authority is built BEFORE the guest subtraction, so a Guest turn would \
         hand a subagent the personal-data groups the turn itself was denied"
    );
    assert!(
        publish_authority > publish_to_shim,
        "the delegation authority is built from an allow-set that is not the one published to \
         the shim; the two can now disagree"
    );

    let call = &code[publish_authority..(publish_authority + 700).min(code.len())];
    assert!(
        call.contains("turn_scope.clone()"),
        "the child's profile scope no longer comes from the turn's resolved scope"
    );
    assert!(
        call.contains("allowed_tools.iter().map(String::as_str)"),
        "the authority's tool set is no longer derived from the turn's allow-set: {call}"
    );
    assert!(
        call.contains("cancel_token.clone()"),
        "the turn's cancellation token is not published, so a child cannot derive one from it"
    );
}

/// The second layer of the child's tool boundary has to be in place before the
/// child's first provider call, or the shim is pass-through for that call and
/// the model is shown tools it may not use.
#[test]
fn the_child_agent_publishes_its_boundary_before_it_replies() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/goose_agent.rs"))
            .expect("goose_agent.rs is next door");
    let code = strip_line_comments(&source);

    let allow = code
        .find("child_controls.set_allowed_tools(plan.allowed_tool_names")
        .expect("the child's allow-set is not published to the shim at all");
    let system = code
        .find("child_controls.set_system_override(Some(plan.system_prompt")
        .expect("the child's system prompt is not claimed, so the shim rebuilds it away");
    let reply = code
        .find(".reply(user_message.clone()")
        .expect("the child loop no longer replies");
    assert!(
        allow < reply && system < reply,
        "the child's boundary is published after its first provider call, so that call is \
         pass-through"
    );

    assert!(
        code.contains("self.shim_controls.forget_session(child_session_id)"),
        "a finished child's shim entry is left to age out; sixty-four of them evict a live \
         parent's allow-set, oldest-first"
    );
}
