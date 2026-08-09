//! PAI-6 P2/P3 guards.
//!
//! The engine drive itself (`GooseAdapter::run_child_agent`) still cannot be
//! run here: it needs a real provider and a real Goose session store. What IS
//! covered is everything that DECIDES — the plan, the turn assembly, the
//! concurrency permit, the outcome classification, the registry and the
//! invariant-2 audit — driven through the real [`GooseOrchestrator::spawn`]
//! against a fake [`ChildRunner`]. The `ChildPlan` the fake receives is produced
//! by production code, so the assertions are about the artifact that actually
//! reaches the engine.
//!
//! The one decision that was NOT out here was how a child's message stream
//! becomes turns and an answer, and that is precisely the one that shipped
//! broken: it lived inline in the drain loop, where nothing without a provider
//! could reach it. [`ChildTurns`] is now beside `classify_outcome` for the same
//! stated reason, and the tests below drive it from a `Vec` of fragments.

use super::*;
use pond_core::models::services::context::model_class::ON_DEVICE_PROVIDERS;
use pond_core::shared::domain::orchestration::{
    AgentRole, DelegationAuthority, RolePersonalData, TaskRequest, DEFAULT_ROLE_MAX_TURNS,
};
use pond_core::shared::services::turn_authority::TurnAuthorityLease;
use pond_core::user_data::domain::profile::ProfileScope;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every test that drives [`GooseOrchestrator::spawn`] takes this first.
///
/// The subagent semaphore is process-wide on purpose — invariant 3 is a
/// property of the device, not of an instance — and `cargo test` runs this file
/// in ONE process with a thread per test. Without serialising,
/// `the_overlap_detector_can_actually_see_overlap` competes for the same three
/// permits as the on-device tests, and its assertion (a REMOTE provider is
/// allowed to overlap) fails whenever an on-device test happens to be holding
/// all three. A vacuity control that fails at random gets deleted, and then the
/// assertion it was protecting proves nothing.
static ONE_RUN_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

// ── A turn is a run of messages, not a message ──────────────────────────────
//
// The regression these exist for made EVERY successful delegation come back
// `TurnBudgetExhausted` with no result, on every streaming provider, which is
// all of them. Goose yields one `AgentEvent::Message` per provider chunk; P2's
// drain loop counted one turn per message and assigned `last_text` per message.

/// One `AgentEvent::Message` as the drain loop sees it.
enum Frag<'a> {
    /// `role == Assistant`, already reduced by `as_concat_text()`.
    Assistant(&'a str),
    /// Any other role. Goose returns a tool response as a `User` message, and
    /// that is the boundary between one provider call and the next.
    ToolResponse,
}

fn assemble(stream: &[Frag<'_>]) -> (Option<String>, u32) {
    let mut turns = ChildTurns::default();
    for fragment in stream {
        match fragment {
            Frag::Assistant(text) => turns.assistant_message(text),
            Frag::ToolResponse => turns.other_role_message(),
        }
    }
    turns.finish()
}

/// The openai-delta and per-token shapes, at their smallest. The assertion is
/// on the CONCATENATION: taking the tail is what P2 did, and the tail of a
/// streamed answer is a word.
#[test]
fn three_fragments_of_one_turn_are_one_turn_and_the_whole_answer() {
    let (answer, turns) = assemble(&[
        Frag::Assistant("It is 18 degrees "),
        Frag::Assistant("and clear "),
        Frag::Assistant("in Nairobi."),
    ]);
    assert_eq!(
        answer.as_deref(),
        Some("It is 18 degrees and clear in Nairobi."),
        "the child's answer is not the concatenation of its fragments - assigning per message \
         hands the parent the last streamed word instead of the message"
    );
    assert_eq!(
        turns, 1,
        "three streamed fragments of one answer were counted as three turns"
    );
}

/// Two provider calls with a tool result between them: two turns, and the
/// answer is the WHOLE of the second, not its tail and not the first.
#[test]
fn a_tool_response_closes_a_turn_and_the_answer_is_the_second_turns_full_text() {
    let (answer, turns) = assemble(&[
        Frag::Assistant("Let me "),
        Frag::Assistant("check the forecast."),
        Frag::ToolResponse,
        Frag::Assistant("It is 18 degrees "),
        Frag::Assistant("and clear."),
    ]);
    assert_eq!(
        answer.as_deref(),
        Some("It is 18 degrees and clear."),
        "the answer is not the whole of the child's LAST turn - a per-message assignment gives \
         its final fragment, and a first-turn answer gives what the child said before it had \
         looked anything up"
    );
    assert_eq!(
        turns, 2,
        "a tool response did not close the turn, so two provider calls counted as one"
    );
}

/// The anthropic shape, and any non-streaming response: one message per
/// complete block. Nothing to concatenate, and it must still be one turn.
#[test]
fn a_single_complete_message_is_one_turn_and_is_the_answer() {
    let (answer, turns) = assemble(&[Frag::Assistant("It is 18 degrees and clear.")]);
    assert_eq!(
        answer.as_deref(),
        Some("It is 18 degrees and clear."),
        "a child whose provider sends one complete block per message lost its answer"
    );
    assert_eq!(
        turns, 1,
        "one complete message was not one turn, so the shape that needs no assembly at all is \
         the one the assembler gets wrong"
    );
}

/// A final turn with no text — every message was `Thinking`, which
/// `as_concat_text()` drops, or a system notification — must not erase the
/// answer that was already given. It is still a turn that happened.
#[test]
fn an_empty_final_turn_does_not_erase_the_answer_before_it() {
    let (answer, turns) = assemble(&[
        Frag::Assistant("It is 18 degrees and clear."),
        Frag::ToolResponse,
        Frag::Assistant(""),
        Frag::Assistant("   "),
    ]);
    assert_eq!(
        answer.as_deref(),
        Some("It is 18 degrees and clear."),
        "a blank final turn overwrote the child's answer with nothing, which classify_outcome \
         then reports as Failed"
    );
    assert_eq!(
        turns, 2,
        "two provider calls were not counted as two turns: either a turn that produced only \
         reasoning did not count at all, or the four messages were counted one apiece"
    );
}

/// **The regression that would have shipped**, at the size the Jetson headline
/// configuration actually produces: local/gguf emits one `AgentEvent::Message`
/// per token piece. Forty of them is a short answer.
///
/// Carried through `classify_outcome` deliberately — the count is not the
/// interesting part on its own, the STATUS is. With one turn per fragment this
/// is 40 > 6 and the user is told their delegation ran out of turns.
#[test]
fn a_per_token_stream_of_forty_fragments_is_one_turn_and_completes() {
    let words: Vec<String> = (0..40).map(|n| format!("w{n} ")).collect();
    let stream: Vec<Frag<'_>> = words.iter().map(|w| Frag::Assistant(w)).collect();
    let (answer, turns) = assemble(&stream);

    assert_eq!(
        turns, 1,
        "a 40-token streamed answer was counted as 40 assistant turns; with a role cap of at \
         most {DEFAULT_ROLE_MAX_TURNS} turns every real delegation is then reported as \
         TurnBudgetExhausted"
    );
    let (status, result) =
        classify_outcome(false, turns, DEFAULT_ROLE_MAX_TURNS, answer.as_deref());
    assert_eq!(
        status,
        TaskStatus::Completed,
        "the delegation answered and was reported as {status:?}"
    );
    assert_eq!(
        result.as_deref(),
        Some(words.join("").trim()),
        "the parent was handed something other than the child's whole answer"
    );
}

/// Vacuity control for the five above: the assembler CAN report nothing and CAN
/// report more than one turn, so `turns == 1` is a measurement rather than the
/// only value this code can produce.
#[test]
fn the_turn_assembler_reports_nothing_for_an_empty_stream() {
    assert_eq!(assemble(&[]), (None, 0));
    assert_eq!(assemble(&[Frag::ToolResponse]), (None, 0));
    let (_, turns) = assemble(&[
        Frag::Assistant("one"),
        Frag::ToolResponse,
        Frag::Assistant("two"),
        Frag::ToolResponse,
        Frag::Assistant("three"),
    ]);
    assert_eq!(turns, 3);
}

/// The goose-side claim the whole rule rests on, read from the submodule rather
/// than remembered: the reply loop treats each yielded message as a FRAGMENT of
/// the current turn and clears the accumulator once per turn.
#[test]
fn goose_still_accumulates_its_own_assistant_text_per_message() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../goose/crates/goose/src/agents/agent.rs"
    ))
    .expect("the goose submodule must be initialised - `git submodule update --init --recursive`");
    assert!(
        source.contains("last_assistant_text.push_str(&text);"),
        "goose no longer accumulates streamed assistant text per message, so the fragment/turn \
         distinction ChildTurns is built on may no longer hold"
    );
    assert!(
        source.contains("turns_taken += 1;"),
        "goose's own turn counter is gone; GIAP's assistant_turns is an approximation of it and \
         the comparison in classify_outcome needs rechecking"
    );
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
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
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
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
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

/// **The instance axis, which every test above is blind to.**
///
/// P2 built a `Semaphore` inside `GooseOrchestrator::new`, so the limit was
/// per-instance while the module doc, the commit message and the PAI-6 document
/// all said process-wide. Every concurrency test built exactly one orchestrator
/// and none could see the difference. The first wiring that constructs one per
/// request — the obvious shape for a handler — would have restored unbounded
/// on-device concurrency silently.
#[tokio::test]
async fn two_orchestrators_cannot_both_run_an_on_device_child() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    // ONE fake engine, so both orchestrators' children are counted by the same
    // in-flight meter - which is the truth of the situation on a pond, where
    // both would be replying through one provider on one GPU.
    let runner = Arc::new(FakeRunner::new(env).holding_for(60));
    let state = runner.state.clone();
    let (first, _turn_a) = live_turn(runner.clone());
    let (second, _turn_b) = live_turn(runner);

    let mut handles = Vec::new();
    for orchestrator in [first, second] {
        let spec = spec_for(&role, &["giap-weather"]);
        handles.push(tokio::spawn(async move { orchestrator.spawn(spec).await }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert_eq!(
        state.max_in_flight.load(Ordering::SeqCst),
        1,
        "two GooseOrchestrator instances each ran an on-device child at the same time, so the \
         subagent semaphore is per-instance rather than process-wide - two subagents then share \
         one GPU and overwrite each other's retained KV prefix, which is what invariant 3 \
         forbids"
    );
}

/// Vacuity control for the test above: two INSTANCES can be observed
/// overlapping when the provider allows it, so `max_in_flight == 1` next door
/// is about the semaphore and not about two orchestrators never being able to
/// run at once in this harness.
#[tokio::test]
async fn two_orchestrators_on_a_remote_provider_do_overlap() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(60));
    let state = runner.state.clone();
    let (first, _turn_a) = live_turn(runner.clone());
    let (second, _turn_b) = live_turn(runner);

    let mut handles = Vec::new();
    for orchestrator in [first, second] {
        let spec = spec_for(&role, &["giap-weather"]);
        handles.push(tokio::spawn(async move { orchestrator.spawn(spec).await }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert!(
        state.max_in_flight.load(Ordering::SeqCst) > 1,
        "two orchestrators on a hosted provider never overlapped, so the on-device assertion \
         next door proves nothing about the semaphore"
    );
}

/// `TaskStatus::Queued` is "authorised, waiting for a concurrency permit", and
/// before this it was never constructed: `spawn` inserted the run as `Running`
/// before acquiring the permit, so a child queued behind an on-device sibling —
/// which is every child after the first — was reported `Running` by `poll` and
/// `list`.
#[tokio::test]
async fn a_child_waiting_for_a_permit_is_queued_rather_than_running() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(200));
    let (orchestrator, _turn) = live_turn(runner);

    let running = spec_for(&role, &["giap-weather"]);
    let running_id = running.id().to_string();
    let waiting = spec_for(&role, &["giap-weather"]);
    let waiting_id = waiting.id().to_string();

    let first = {
        let orchestrator = orchestrator.clone();
        tokio::spawn(async move { orchestrator.spawn(running).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    let second = {
        let orchestrator = orchestrator.clone();
        tokio::spawn(async move { orchestrator.spawn(waiting).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;

    let ahead = orchestrator
        .poll(&running_id)
        .await
        .unwrap()
        .expect("known");
    let behind = orchestrator
        .poll(&waiting_id)
        .await
        .unwrap()
        .expect("known");
    assert_eq!(
        behind.status,
        TaskStatus::Queued,
        "a child that has not been given a concurrency permit reported itself as {:?}; Queued is \
         the one state that variant exists to distinguish and nothing constructed it",
        behind.status
    );
    // Vacuity control, inline: the run AHEAD of it is Running at the same
    // instant, so Queued is a distinction and not what every run says.
    assert_eq!(
        ahead.status,
        TaskStatus::Running,
        "the run holding the permit was not reported as Running, so the assertion above is not \
         about queueing"
    );

    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(
        orchestrator
            .poll(&waiting_id)
            .await
            .unwrap()
            .expect("known")
            .status,
        TaskStatus::Completed,
        "the queued run never left Queued, so the transition when the permit arrives is missing"
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

/// A fixture production can actually produce — which the version of this test
/// that shipped with P2 was not, while its own doc comment asserted that it was.
///
/// The premise is right: the parent's strip is a best-effort
/// `remove_extension(...).ok()`, so a parent whose strip failed really is still
/// listing a stripped builtin's tools. The fixture was wrong. It used
/// `developer__shell` and `summon__delegate`, and `PLATFORM_EXTENSIONS` marks
/// both of those extensions `unprefixed_tools: true` — `extension_manager.rs`'s
/// `is_unprefixed_extension` then exposes their tools as bare `shell` and
/// `delegate`, never prefixed, so [`split_extension_tool`] drops them before
/// `parent_tools` exists and this function was never reached with such a name.
/// That is this programme's recorded shape 6, a fixture no code path can
/// produce, and it means the test was proving nothing about `grants_tool`.
///
/// `todo`, `orchestrator`, `apps`, `extensionmanager`, `summarize`,
/// `computercontroller` and `tom` are the stripped builtins whose tools ARE
/// prefixed, so those are what a failed strip actually leaves in `list_tools`.
/// Pinned against the submodule by
/// [`the_goose_builtins_that_expose_bare_tool_names_are_still_these`].
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
        ("todo", &["todo_write"]),
        ("orchestrator", &["start_agent", "send_message"]),
    ]);

    let configs = child_extensions(&spec, &tools).unwrap();
    let names: Vec<String> = configs.iter().map(extension_name).collect();
    for stripped in GOOSE_STRIPPED_BUILTINS {
        assert!(
            !names.iter().any(|n| n == stripped),
            "the child was handed `{stripped}`, one of the ten builtins GIAP strips from every \
             session - `orchestrator` is how a subagent drives another agent"
        );
    }
    // The positive half, so the assertion above cannot be satisfied by an empty
    // result: the child still got exactly the extension it was authorised for.
    assert_eq!(
        names,
        vec!["giap-weather".to_string()],
        "the refusal above is passing because nothing came out at all"
    );
}

/// The other half of the claim the fixture above rests on: a bare tool name
/// belongs to no extension, so a failed strip of `developer` or `summon` cannot
/// even reach the plan builder.
#[test]
fn a_bare_tool_name_belongs_to_no_extension_and_cannot_enter_a_plan() {
    for bare in ["shell", "delegate", "final_output", "text_editor", ""] {
        assert_eq!(
            split_extension_tool(bare),
            None,
            "`{bare}` was given an owning extension; goose exposes the unprefixed platform \
             extensions' tools under exactly these names and guessing an owner for one would \
             invent an extension the engine never named"
        );
    }
    // Vacuity control: the function does resolve a real prefixed name, so the
    // Nones above are about the shape and not about the function never
    // answering.
    assert_eq!(
        split_extension_tool("giap-weather__get_weather"),
        Some(("giap-weather", "get_weather"))
    );
    assert_eq!(
        split_extension_tool("todo__todo_write"),
        Some(("todo", "todo_write"))
    );
    // A separator with nothing in front of it names no extension either.
    assert_eq!(split_extension_tool("__orphan"), None);
}

/// Which stripped builtins expose BARE tool names, read from the submodule
/// rather than remembered.
///
/// The fixture two tests up models a failed strip, and whether it models one
/// depends entirely on this. If a Goose sync flips `unprefixed_tools` on any of
/// these, the fixture silently stops being producible again — which is the
/// defect this test exists to make loud.
#[test]
fn the_goose_builtins_that_expose_bare_tool_names_are_still_these() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../goose/crates/goose/src/agents/platform_extensions/mod.rs"
    ))
    .expect("the goose submodule must be initialised - `git submodule update --init --recursive`");

    for bare in ["developer", "summon"] {
        assert!(
            goose_extension_is_unprefixed(&source, bare),
            "goose no longer exposes `{bare}`'s tools unprefixed, so a parent whose strip failed \
             now lists `{bare}__...` and the failed-strip fixture must use it"
        );
    }
    for prefixed in ["todo", "orchestrator"] {
        assert!(
            !goose_extension_is_unprefixed(&source, prefixed),
            "goose now exposes `{prefixed}`'s tools unprefixed, so the failed-strip fixture uses \
             a name production can no longer produce"
        );
    }
}

/// Read one `PLATFORM_EXTENSIONS` entry's `unprefixed_tools` flag out of goose's
/// source. The map is a literal, so the entry is the text between this
/// extension's `map.insert(` key and the next one.
fn goose_extension_is_unprefixed(source: &str, extension: &str) -> bool {
    let key = format!("{extension}::EXTENSION_NAME,");
    let at = source
        .find(&key)
        .unwrap_or_else(|| panic!("goose's PLATFORM_EXTENSIONS no longer registers `{extension}`"));
    let entry = &source[at..];
    let end = entry.find("map.insert(").unwrap_or(entry.len());
    entry[..end].contains("unprefixed_tools: true")
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
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
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
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
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
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
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
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
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
    let plan = build_child_plan(&spec, "child-1", &env, None).unwrap();

    assert!(
        plan.system_prompt.starts_with(&env.base_system_prefix),
        "the child's prompt must start from the SAME `build_prompt_partition` prefix the parent \
         uses; the phase bullet said to render `subagent_system.md`, which is deleted"
    );
    assert!(
        plan.system_prompt
            .contains("You are the `researcher` helper."),
        "the role is not named to the child"
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

/// **Half of a two-half fix, and said plainly rather than dressed up.**
///
/// A role's stored `instructions` are its persona. `AgentRole` validates them as
/// REQUIRED — there is a `MissingInstructions` error for their absence — and
/// then nothing outside one `pond-core` unit test reads them, so a role has been
/// a name plus a tool list plus a turn budget, and the envelope printed the
/// name. The guard next door could not see that: `contains("researcher")` is
/// satisfied by the name alone.
///
/// The adapter half is here and is real: given a persona, the envelope renders
/// it under the heading the child is already told to read. The other half is
/// carrying it onto `TaskSpec`, which is a `pond-core` change this commit does
/// not own, so `GooseOrchestrator::spawn` passes `None` and this fixture is one
/// production cannot yet produce — recorded shape 6, named here so nobody reads
/// this as proof that a role's persona reaches a child today. It does not. What
/// makes it survivable is that the missing half is one accessor
/// (`TaskSpec::role_instructions`), not an unwritten design, and this test
/// becomes the guard for the whole path the moment that accessor exists.
#[test]
fn the_envelope_renders_a_roles_persona_when_it_is_given_one() {
    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let persona = "You check the forecast and answer in one sentence, in Celsius.";

    let with_persona = build_child_plan(&spec, "child-1", &env, Some(persona)).unwrap();
    assert!(
        with_persona.system_prompt.contains(persona),
        "the role's stored instructions did not reach the child's prompt, so a role is still \
         just a name plus a tool list"
    );
    assert!(
        with_persona
            .system_prompt
            .contains("You are the `researcher` helper."),
        "the persona replaced the role's name instead of following it"
    );
    // The task is the child's user message, never the persona, or a role's
    // standing character would be rewritten by every delegation.
    assert_eq!(with_persona.user_message, "what is the weather");

    // Vacuity control: the assertion above is about the persona being rendered,
    // not about the prompt containing that sentence anyway.
    let without = build_child_plan(&spec, "child-1", &env, None).unwrap();
    assert!(!without.system_prompt.contains(persona));
    assert!(without
        .system_prompt
        .contains("You are the `researcher` helper."));
}

#[test]
fn a_child_with_no_tools_is_told_so_rather_than_shown_an_empty_list() {
    let role = role("summariser", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    // Authorised for giap-weather, but the parent has nothing loaded.
    let env = env_with("ollama", parent_tools(&[]));
    let plan = build_child_plan(&spec, "child-1", &env, None).unwrap();
    assert!(plan.extensions.is_empty());
    assert!(plan.system_prompt.contains("no tools on this run"));
}

// ── The registry ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_finished_run_can_be_polled_and_listed_and_an_unknown_one_cannot() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
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
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
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
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
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
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
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
    let plan = build_child_plan(&spec, "child-1", &env, None).unwrap();

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

// ── Tripwires over the child loop ───────────────────────────────────────────
//
// These read source text, and that is a weak shape this programme has recorded:
// a grep catches a textual revert and nothing subtler. They are here because
// `run_child_agent` needs a provider and a Goose session store, so the ordering
// facts below cannot be observed any other way, and each of them was WRONG in
// the version that shipped. Read them as tripwires, not as coverage — the
// behaviour they are about is covered by the `ChildTurns` tests above, which do
// not read source at all.

/// Everything about turns must go through [`ChildTurns`], which is where the
/// rule lives and where it is tested. A counter incremented per message inside
/// the drain loop is exactly the defect: one `AgentEvent::Message` is a provider
/// chunk.
#[test]
fn the_child_drain_loop_counts_turns_rather_than_fragments() {
    let child_loop = child_loop_source();

    assert!(
        child_loop.contains("ChildTurns::default()"),
        "the child drain loop no longer assembles turns through ChildTurns, so whatever it does \
         instead is untested: nothing without a provider can reach that loop"
    );
    assert!(
        child_loop.contains("turns.assistant_message("),
        "assistant messages are no longer accumulated into a turn"
    );
    assert!(
        child_loop.contains("turns.other_role_message()"),
        "nothing closes a turn, so a tool response no longer separates two provider calls and \
         the whole run reads as one turn"
    );
    assert!(
        !child_loop.contains("assistant_turns = assistant_turns"),
        "the drain loop increments a per-message turn counter again; goose yields one message \
         per provider chunk, so this reports every streamed answer as TurnBudgetExhausted"
    );
    assert!(
        !child_loop.contains("last_text = Some("),
        "the drain loop assigns last_text per message again; on any streaming provider that is \
         the last fragment, not the last message"
    );
}

/// Invariant 2's audit has to see the state the child RAN with. Read before
/// `reply`, it can only report what `add_extension` was handed one line
/// earlier — which `child_extensions` had already refused — so it was an audit
/// structurally incapable of failing.
#[test]
fn the_invariant_two_audit_reads_the_child_after_it_has_run() {
    let child_loop = child_loop_source();
    let reply = child_loop
        .find(".reply(user_message.clone()")
        .expect("the child loop no longer replies");
    let audits: Vec<usize> = child_loop
        .match_indices("list_extensions()")
        .map(|(at, _)| at)
        .collect();

    assert!(
        !audits.is_empty(),
        "the child's loaded extensions are never read, so invariant 2 has no post-run audit at \
         all"
    );
    assert!(
        audits.iter().any(|at| *at > reply),
        "every read of the child's extension list happens BEFORE it replies, so the audit only \
         ever sees what add_extension was just given - which the plan builder had already \
         refused"
    );
}

/// PAI-4 P5. A child replies through the same engine and overwrites the one
/// retained KV prefix; if nothing says so, the parent's next turn finds its
/// hashes equal, takes the unchanged branch and records the prefix WARM when it
/// is cold.
#[test]
fn a_child_run_tells_the_prefix_cache_that_it_moved_the_prefix() {
    let child_loop = child_loop_source();
    let reply = child_loop
        .find(".reply(user_message.clone()")
        .expect("the child loop no longer replies");
    let noted = child_loop.find("note_prefix_invalidated").expect(
        "a child run no longer tells the prefix-cache state machine anything, so the \
                parent's next turn will record a cold prefix as warm and PAI-4's age rung will \
                not fire",
    );
    assert!(
        noted < reply,
        "the prefix invalidation is recorded after the child replies; a reply that fails \
         part-way has still prefilled, and this programme's rule is that the failure direction \
         narrows"
    );
}

/// `run_child_agent`'s body, so a tripwire above cannot be satisfied by
/// something elsewhere in a 4,700-line file. Comments are stripped for the same
/// reason they are stripped in the guard next door: a check satisfied by prose
/// is this programme's recorded shape 1.
fn child_loop_source() -> String {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/goose_agent.rs"))
            .expect("goose_agent.rs is next door");
    let code = strip_line_comments(&source);
    let at = code
        .find("pub(crate) async fn run_child_agent")
        .expect("GooseAdapter no longer owns the child loop");
    let body = &code[at..];
    // The first closing brace in column zero after the method is the end of the
    // `impl GooseAdapter` block it is the last member of. Everything inside is
    // indented, so this cannot end early.
    let end = body.find("\n}\n").map(|at| at + 3).unwrap_or(body.len());
    body[..end].to_string()
}
