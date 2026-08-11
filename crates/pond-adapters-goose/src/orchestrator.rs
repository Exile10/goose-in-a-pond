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
//! | 2 — `summon`/`orchestrator`/`todo` stay stripped | [`GOOSE_STRIPPED_BUILTINS`] is now the single source for `goose_agent.rs`'s strip list, this file's plan-time refusal, and the post-run audit in [`stripped_builtins_present`] — which is read AFTER the drain, not before the run |
//! | 3 — concurrency 1 on this device | ONE process-wide [`Semaphore`] ([`process_subagent_permits`]), acquired with [`subagent_permits`] permits on the only path that can start a child, AND with [`parent_turn_permits`] by every on-device parent turn ([`claim_device_for_turn`]) — P4, because a child does not queue behind a parent's provider call, it overwrites the one retained KV prefix. A synchronous child INHERITS its own parent's claim rather than deadlocking against it, and inheriting means taking the parent hold's own one-permit semaphore, so SIBLINGS of one delegating turn still run one at a time |
//! | 3b — a subagent is a second claim on one window | [`DeviceLedger`] holds each live child's `context_fraction` for as long as the run does, and `GooseAdapter::turn_profile` shrinks the parent's `history_token_budget` — and only that — by what it finds there |
//! | 4 — child turns never reach the parent's history | the drain loop keeps only the text of the child's last completed assistant TURN ([`ChildTurns`]), and [`ChildRunner::release`] deletes the child's engine session afterwards |
//! | 5 — cancellable, and a parent cancels its children | one [`CancellationToken`] per run in the registry. A SYNCHRONOUS run's is derived from the parent TURN's, so the chat stream's `DropGuard` cancels it; a BACKGROUND run's is its own, owned by the parent SESSION, because a run that outlives the turn cannot inherit a token that dies with it — its cascade is [`Orchestrator::cancel_children_of`], which is implemented here and still has no production caller (see P8) |
//! | 6 — depth capped | structural, and NOT by withholding a GIAP extension: **there is no delegation tool in this tree.** Goose's own one is `summon__delegate`, which [`GOOSE_STRIPPED_BUILTINS`] refuses at plan time and which [`child_extensions`] could not emit anyway, since it only ever emits what the PARENT already had loaded. P1 refuses at [`MAX_DELEGATION_DEPTH`](pond_core::shared::domain::orchestration::MAX_DELEGATION_DEPTH) before a spec exists at all, and the envelope tells the child in words |
//!
//! # What this deliberately does not do
//!
//! The RESULT does not stream. `spawn` runs a synchronous child to completion
//! and returns a terminal [`TaskRun`].
//!
//! **That paragraph promised `poll` "exists so P8 can add a background path
//! without changing the port", and P8 landed on 2026-08-11 having done exactly
//! that** — the port is untouched, `TaskStatus` gained no variant, and a
//! background `spawn` returns a `Queued` run whose progress is read back through
//! `poll`. It is refused outright on any provider that runs on this device
//! ([`BackgroundAvailability`]), which is the same invariant 3 the semaphore
//! enforces: where one agent may hold the model at a time, "background" is just
//! the parent's next turn waiting.
//!
//! Per-role models are P7, and are refused on the same providers for the
//! neighbouring reason — see [`child_model_config`].
//!
//! **This paragraph said "it does not stream" until P6 landed on 2026-08-10**,
//! and that is now only true of the result. [`ProgressBus`] carries lifecycle
//! and tool-name frames out to the parent's chat stream while a run is in
//! flight — deliberately NOT through the port, whose return type is the one
//! string invariant 4 allows a parent to take back.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::mcp::domain::tool_group::TOOL_NAME_SEPARATOR;
use pond_core::shared::domain::agent::{AgentStreamEvent, SubagentStatus};
use pond_core::shared::domain::orchestration::{
    max_concurrent_subagents, BackgroundAvailability, ChildModel, TaskRun, TaskSpec, TaskStatus,
    REMOTE_SUBAGENT_CONCURRENCY,
};
use pond_core::shared::ports::orchestrator::Orchestrator;
use pond_core::shared::services::turn_authority::TurnAuthorityRegistry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
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
/// **The predicate is `max_concurrent_subagents`, which asks
/// `provider_locality`** — `local`, `gguf`, `ollama` and `llamafile` run here,
/// and so does any name in neither of its two lists. Ollama and llamafile speak
/// HTTP, but on a GIAP pond they speak it to `127.0.0.1`, which is the same GPU
/// the parent's next turn needs; PAI-4 P2 already had to fix the narrow
/// `local`/`gguf` reading once. A provider nothing can place gets the same
/// answer, because "not on the deny-list" is not evidence that the work happens
/// somewhere else.
///
/// `div_ceil` rather than `/` so an awkward ratio errs toward LESS concurrency.
/// With `SUBAGENT_PERMITS = 3`: on-device asks for 3 of 3 (concurrency 1),
/// remote asks for 1 of 3 (concurrency 3).
pub fn subagent_permits(provider: &str) -> u32 {
    let concurrent = max_concurrent_subagents(provider).max(1);
    SUBAGENT_PERMITS.div_ceil(concurrent) as u32
}

/// The one subagent semaphore on this pond.
///
/// **Process-wide, and deliberately not a constructor argument.** P2 built a
/// fresh `Semaphore` inside [`GooseOrchestrator::new`] while the module doc, the
/// commit and the PAI-6 document all said "process-wide", and no test could see
/// the difference because every test built one orchestrator. The first wiring
/// that constructs an orchestrator per request — the obvious thing to write for
/// a handler — would then have restored unbounded on-device concurrency, which
/// is exactly what invariant 3 forbids, silently and with the doc still claiming
/// otherwise.
///
/// I preferred a visible constructor argument and then argued myself out of it.
/// Invariant 3 is a property of the DEVICE — one GPU, one retained KV prefix —
/// so it must not be expressible per instance. An `Arc<Semaphore>` parameter
/// makes the limit a convention each caller has to honour, and this programme's
/// standing rule is that a widening default is a bug: `Semaphore::new(3)` at a
/// call site would look exactly like correct wiring. There is no argument to get
/// wrong here, and no way to construct a [`GooseOrchestrator`] with a private
/// one.
pub fn process_subagent_permits() -> Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(SUBAGENT_PERMITS)))
        .clone()
}

/// Permits a PARENT turn must hold before it touches the engine — PAI-6 P4.
///
/// # Why a parent turn takes permits at all
///
/// P2 closed half of invariant 3: subagents are serialised against each other.
/// Section 3.4 says they must also be serialised against the parent's turns,
/// and the mechanism is not a metaphor. `goose-local-inference`'s `LoadedModel`
/// holds `session: Option<SessionKv>` — exactly ONE retained KV prefix per
/// loaded model slot, process-wide. A child replying between two of the
/// parent's provider calls does not queue behind the parent, it OVERWRITES the
/// prefix, and the parent pays a full re-prefill (3.7 s on the Orin) on its
/// next call.
///
/// # Why the whole semaphore, or none of it
///
/// The hazard is one physical model slot, so it exists only where the model is.
/// A provider that runs somewhere else has no shared prefix to lose and its
/// parent turns take nothing — four hosted conversations still run four
/// abreast, exactly as they do today. On this device a turn takes every permit,
/// which is the same claim [`subagent_permits`] gives an on-device child: one
/// thing at a time, parent or child.
///
/// The predicate is [`max_concurrent_subagents`], not a name list, for the
/// reason PAI-4 P2 had to fix once: ollama and llamafile speak HTTP to
/// `127.0.0.1` and are on this device. It answers 1 for a provider it cannot
/// place too, so an unrecognised name makes a parent turn take the whole
/// semaphore rather than none of it.
pub fn parent_turn_permits(provider: &str) -> u32 {
    if max_concurrent_subagents(provider) <= 1 {
        SUBAGENT_PERMITS as u32
    } else {
        0
    }
}

// ── The live-delegation ledger (PAI-6 P4) ───────────────────────────────────

/// What live delegations are currently claiming from their parents: a share of
/// the history budget, and the device itself.
///
/// **Process-wide, and obtained only through [`process_device_ledger`]**, for
/// the reason recorded on [`process_subagent_permits`]: both facts here are
/// properties of the DEVICE and of a conversation, not of whichever
/// `GooseOrchestrator` instance a handler happened to build. A per-instance
/// ledger would read the same at every call site and silently answer "no child
/// is live" for a child another instance is running.
///
/// It is also the seam the adapter reads WITHOUT depending on the orchestrator:
/// `GooseChildRunner` holds an `Arc<GooseAdapter>`, so an adapter that asked a
/// `GooseOrchestrator` what was reserved would close a cycle.
#[derive(Default)]
pub struct DeviceLedger {
    /// GIAP session id -> the `context_fraction` of each live child of it.
    ///
    /// A `Vec` rather than a running sum: releasing a child must subtract
    /// exactly what it added, and repeated f32 addition and subtraction does
    /// not return to zero. A parent whose budget never fully came back would
    /// lose recall for the rest of the conversation, silently.
    reservations: Mutex<HashMap<String, Vec<f32>>>,
    /// GIAP session id -> its live turns' hold on the device.
    device_holders: Mutex<HashMap<String, DeviceHoldState>>,
}

/// One session's device hold, and the semaphore its children share.
struct DeviceHoldState {
    /// How many live turns of this session hold the device permit.
    ///
    /// A count rather than a flag because one session can have two streams
    /// open (a voice turn and a chat turn), and the second one ending must not
    /// tell `spawn` that the first has released the device.
    holders: usize,
    /// ONE permit, taken by every child that INHERITS this hold.
    ///
    /// P4 gave an inheriting child `needed = 0`, and `acquire_many_owned(0)`
    /// never blocks. But a device hold is a property of the SESSION, so every
    /// child of the delegating turn inherited it: N concurrent delegations
    /// from one parent turn all ran at once on the one GPU, overwriting each
    /// other's retained KV prefix. That is invariant 3 with its on-device half
    /// deleted, by the very pass written to avoid the deadlock — and the
    /// ledger already models the state it breaks in, since `reservations`
    /// holds a `Vec` of live children per session.
    ///
    /// A child must not queue behind its own PARENT (which is blocked inside a
    /// tool call, not talking to the provider) and must still queue behind its
    /// SIBLINGS. Those are two different questions, so they are two different
    /// semaphores rather than two readings of one.
    children: Arc<Semaphore>,
}

/// The one ledger on this pond. See [`DeviceLedger`] for why it is not a
/// constructor argument.
pub fn process_device_ledger() -> Arc<DeviceLedger> {
    static LEDGER: OnceLock<Arc<DeviceLedger>> = OnceLock::new();
    LEDGER
        .get_or_init(|| Arc::new(DeviceLedger::default()))
        .clone()
}

impl DeviceLedger {
    fn lock_reservations(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<f32>>> {
        self.reservations.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_holders(&self) -> std::sync::MutexGuard<'_, HashMap<String, DeviceHoldState>> {
        self.device_holders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Claim `fraction` of `parent_session_id`'s history budget until the
    /// returned guard drops.
    pub fn reserve(self: &Arc<Self>, parent_session_id: &str, fraction: f32) -> HistoryReservation {
        self.lock_reservations()
            .entry(parent_session_id.to_string())
            .or_default()
            .push(fraction);
        HistoryReservation {
            ledger: Arc::clone(self),
            session_id: parent_session_id.to_string(),
            fraction,
        }
    }

    /// How much of this session's history budget its live children hold.
    ///
    /// Clamped to 1.0: three children at 0.5 each cannot take 150% of a window
    /// that does not have it, and the honest answer at that point is "all of
    /// it", which `CompactionProfile::with_history_reserved` floors at
    /// `MIN_HISTORY_TOKENS` rather than at zero.
    pub fn reserved_fraction(&self, giap_session_id: &str) -> f32 {
        self.lock_reservations()
            .get(giap_session_id)
            .map(|fractions| fractions.iter().sum::<f32>().clamp(0.0, 1.0))
            .unwrap_or(0.0)
    }

    /// Record that a turn of `giap_session_id` holds the device permit.
    fn hold_device(self: &Arc<Self>, giap_session_id: &str) -> DeviceHold {
        let mut holders = self.lock_holders();
        let state = holders
            .entry(giap_session_id.to_string())
            .or_insert_with(|| DeviceHoldState {
                holders: 0,
                // One. Siblings of one delegating turn serialise against each
                // other exactly as any two on-device children do.
                children: Arc::new(Semaphore::new(1)),
            });
        state.holders += 1;
        drop(holders);
        DeviceHold {
            ledger: Arc::clone(self),
            session_id: giap_session_id.to_string(),
        }
    }

    /// Whether a live turn of this session already holds the device permit.
    ///
    /// Diagnostics and tests. **`spawn` deliberately does not read this**: a
    /// `bool` answers "may this child skip the queue" when the question is
    /// *which* queue, and answering the first is what let every child of one
    /// delegating turn run at once. [`inherited_child_permits`](Self::inherited_child_permits)
    /// answers the second.
    pub fn session_holds_device(&self, giap_session_id: &str) -> bool {
        self.lock_holders()
            .get(giap_session_id)
            .is_some_and(|state| state.holders > 0)
    }

    /// The semaphore an inheriting child of this session must take a permit
    /// from, or `None` when no live turn of it holds the device.
    ///
    /// `Some` is not a free pass. It is a semaphore of ONE, so a child never
    /// queues behind the parent that spawned it and always queues behind its
    /// siblings — see [`DeviceHoldState::children`], which is the regression
    /// this returns a semaphore rather than a `bool` to prevent.
    pub fn inherited_child_permits(&self, giap_session_id: &str) -> Option<Arc<Semaphore>> {
        self.lock_holders()
            .get(giap_session_id)
            .filter(|state| state.holders > 0)
            .map(|state| Arc::clone(&state.children))
    }

    /// Live children of this session. Diagnostics and tests only.
    pub fn live_children(&self, giap_session_id: &str) -> usize {
        self.lock_reservations()
            .get(giap_session_id)
            .map(Vec::len)
            .unwrap_or(0)
    }
}

/// One live child's claim on its parent's history budget. Dropping it releases.
///
/// Deliberately has no release method and no way to change the fraction: the
/// run owns it, and a run that ends — normally, by cancellation, by the plan
/// being refused, or by the future being dropped — gives the budget back
/// without anything having to remember to.
pub struct HistoryReservation {
    ledger: Arc<DeviceLedger>,
    session_id: String,
    fraction: f32,
}

impl Drop for HistoryReservation {
    fn drop(&mut self) {
        let mut reservations = self.ledger.lock_reservations();
        if let Some(fractions) = reservations.get_mut(&self.session_id) {
            if let Some(at) = fractions.iter().position(|f| *f == self.fraction) {
                fractions.remove(at);
            }
            if fractions.is_empty() {
                reservations.remove(&self.session_id);
            }
        }
    }
}

/// Records that a session's turn holds the device. Dropping it clears the
/// record.
struct DeviceHold {
    ledger: Arc<DeviceLedger>,
    session_id: String,
}

impl Drop for DeviceHold {
    fn drop(&mut self) {
        let mut holders = self.ledger.lock_holders();
        let empty = match holders.get_mut(&self.session_id) {
            Some(state) => {
                state.holders = state.holders.saturating_sub(1);
                state.holders == 0
            }
            None => false,
        };
        if empty {
            // A child that inherited this hold and is still running keeps its
            // own `Arc` of the sub-semaphore, so removing the entry only stops
            // a LATER child inheriting a hold nobody has. That window — a
            // parent's stream dropped by the client while its child runs on —
            // is the same one inheritance has always had, and it narrows: the
            // next child acquires from the process semaphore instead.
            holders.remove(&self.session_id);
        }
    }
}

/// A parent turn's exclusive claim on this device, held for the whole turn.
///
/// Both fields are held for their `Drop`. Order matters and is the declaration
/// order: the ledger record is cleared before the permit is returned, so there
/// is no instant in which the permit is free while `session_holds_device` still
/// says a turn of that session is holding it — which is the window in which a
/// child would inherit a permit nobody has.
pub struct TurnDeviceClaim {
    _hold: DeviceHold,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Take the device for a parent turn, or return `None` when there is nothing
/// to take.
///
/// `None` means "this turn holds no claim" and has two causes the caller must
/// treat identically: the provider does not run on this device (so there is no
/// shared prefix to protect), or the turn was cancelled while queueing. In the
/// second case the reply that follows sees the same cancelled token and exits
/// immediately, so proceeding without a claim is correct rather than a
/// fallback.
///
/// # The deadlock this must not create
///
/// A synchronous delegation runs INSIDE its parent's turn: the parent is
/// blocked in a tool call, not talking to the provider. If the child queued for
/// the same permits its parent is holding, neither would ever finish — and
/// because a delegating turn also holds one of `main.rs`'s four `sse_semaphore`
/// permits, four such turns would exhaust the interactive chat pool for the
/// lifetime of the process, with every later stream answered 503. So a child
/// whose parent's turn already holds the device INHERITS it rather than
/// acquiring; see `GooseOrchestrator::spawn`.
pub async fn claim_device_for_turn(
    giap_session_id: &str,
    provider: &str,
    cancel: &CancellationToken,
) -> Option<TurnDeviceClaim> {
    let needed = parent_turn_permits(provider);
    if needed == 0 {
        return None;
    }
    let permits = process_subagent_permits();
    let permit = tokio::select! {
        biased;
        _ = cancel.cancelled() => return None,
        acquired = permits.acquire_many_owned(needed) => acquired.ok()?,
    };
    // The record is taken only once the permit is actually held: a turn that is
    // still queueing has not got the device, and a child that read otherwise
    // would skip acquiring and run alongside whoever does hold it.
    let hold = process_device_ledger().hold_device(giap_session_id);
    Some(TurnDeviceClaim {
        _hold: hold,
        _permit: permit,
    })
}

// ── The progress channel (PAI-6 P6) ─────────────────────────────────────────

/// One thing a live delegation did, on its way to its parent's chat stream.
///
/// The adapter-side twin of
/// [`AgentStreamEvent::SubagentProgress`](pond_core::shared::domain::agent::AgentStreamEvent::SubagentProgress),
/// carrying the same four fields plus the routing key. It exists as its own
/// type because the bus has to know which parent a frame is for and the stream
/// event deliberately does not: a client is told about the delegation it is
/// watching, never which session it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildProgress {
    /// The GIAP session id of the parent turn that authorised the run. This is
    /// `TaskSpec::parent_session_id`, which is what `DeviceLedger` and
    /// `TurnAuthorityRegistry::parent_turn_token` already key on.
    pub parent_session_id: String,
    pub task_id: String,
    pub role: String,
    pub status: SubagentStatus,
    /// A tool NAME, or a reason GIAP itself wrote. **Never the child's own
    /// words** — see [`child_tool_names`], which is the only producer of the
    /// tool-name case and cannot express anything else.
    pub detail: Option<String>,
}

impl From<ChildProgress> for AgentStreamEvent {
    fn from(progress: ChildProgress) -> Self {
        AgentStreamEvent::SubagentProgress {
            task_id: progress.task_id,
            role: progress.role,
            status: progress.status,
            detail: progress.detail,
        }
    }
}

/// Where a progress frame crosses from a child's loop to its parent's stream.
///
/// # The problem this solves, which is the whole of P6
///
/// A synchronous delegation runs INSIDE its parent's turn. The parent is parked
/// on `goose_stream.next().await`; underneath that await, Goose is dispatching
/// the `delegate` tool call, which is running the child to completion. So the
/// parent's stream yields nothing at all between the tool call and its result —
/// minutes, on-device — and a frame produced in the child's loop has no path to
/// it. It needs a side channel that the parent's drain also selects on.
///
/// # What was chosen, and what was rejected
///
/// An unbounded `mpsc` per live turn, held in a process-wide map keyed by the
/// parent's GIAP session id, with the parent's drain doing a `biased` select on
/// the receiver first and the engine second. Unbounded because the alternative
/// is a bounded channel whose `send` can block, and the sender here is the child
/// loop running underneath the parent's own poll — a full channel would deadlock
/// the very future that drains it. The volume is bounded anyway: a frame per
/// lifecycle transition plus one per tool call, against a role capped at twelve
/// turns.
///
/// Rejected, in order of how tempting they were:
///
/// - **`SubagentRunParams::on_message` / `notification_tx`.** What section 3.7
///   of the PAI-6 document told five previous attempts to use. Both are fields
///   of the parameter struct of `run_subagent_task`, which lives behind
///   `pub(crate) mod subagent_handler` and which P2 deliberately does not call.
///   Worse, `notification_tx` is also a live GIAP symbol — the broadcast behind
///   `GET /notifications/stream` in `main.rs` — so grepping the name finds a
///   real channel that has nothing to do with this.
/// - **A `tokio::sync::broadcast` instead of per-turn `mpsc`.** One channel
///   every turn subscribes to means every frame reaches every live stream and
///   each one filters by session. A filter is a thing that can be got wrong;
///   this programme's rule is that access narrows on failure, and a missing
///   filter would show one household member's delegation inside another's chat.
///   Routing at the map, so a frame is only ever *handed to* one subscriber, is
///   the narrower failure.
/// - **Threading a `Sender` down through `TaskSpec` and `ChildPlan`.** The spec
///   is `pond-core`'s authorisation artifact, built by
///   `DelegationAuthority::delegate`, and putting a transport handle on it would
///   both invert the dependency and hand every future producer of a spec a
///   channel to fill in. The plan is the security artifact and is asserted on
///   whole by the tests; a non-comparable field on it costs those assertions.
/// - **Keying on the engine (Goose) session id instead.** It is the better key
///   for an *authorisation* question, which is why `TurnAuthorityRegistry` uses
///   it, but a `TaskSpec` carries the GIAP id and the mapping is 1:1
///   (`engine_session_map.session_id` is the primary key). A second lookup would
///   buy nothing and add a way for the two to disagree.
///
/// # What the key does NOT protect against
///
/// Two concurrent turns of the SAME GIAP session. The newest subscriber wins
/// and the older turn's children lose their frames — frames, not results. That
/// is the same coarseness `DeviceLedger` and `parent_turn_token` already have,
/// and it stays inside one session, so it can never cross the profile boundary
/// PAI-1 draws. It is a lost frame, never a misdelivered one.
#[derive(Default)]
pub struct ProgressBus {
    subscribers: Mutex<HashMap<String, Subscriber>>,
    next_id: std::sync::atomic::AtomicU64,
}

/// One live parent turn's end of the channel.
struct Subscriber {
    /// Identity, so a lease that drops AFTER a newer turn subscribed removes
    /// its own entry and not the newer one's.
    id: u64,
    tx: tokio::sync::mpsc::UnboundedSender<ChildProgress>,
}

/// The one bus on this pond.
///
/// Process-wide and not a constructor argument, for the reason recorded on
/// [`process_subagent_permits`]: an orchestrator built per request would
/// otherwise publish into a bus nobody is listening to, which is a feature that
/// silently does nothing — this programme's signature failure.
pub fn process_progress_bus() -> Arc<ProgressBus> {
    static BUS: OnceLock<Arc<ProgressBus>> = OnceLock::new();
    BUS.get_or_init(|| Arc::new(ProgressBus::default())).clone()
}

impl ProgressBus {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Subscriber>> {
        self.subscribers.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Listen for the delegations of `giap_session_id` until the returned
    /// stream is dropped.
    pub fn subscribe(self: &Arc<Self>, giap_session_id: &str) -> ProgressStream {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.lock().insert(
            giap_session_id.to_string(),
            Subscriber { id, tx: tx.clone() },
        );
        ProgressStream {
            rx,
            _keepalive: tx,
            bus: Arc::downgrade(self),
            session_id: giap_session_id.to_string(),
            id,
        }
    }

    /// Hand a frame to the live turn it belongs to, or drop it.
    ///
    /// Dropping is the ordinary case, not an error: a delegation started by the
    /// CLI, by a recipe run, or by a client that hung up mid-turn has no stream
    /// to reach. It is deliberately silent — a `warn!` per frame would fill the
    /// log of every headless delegation.
    pub fn publish(&self, frame: ChildProgress) {
        if let Some(subscriber) = self.lock().get(&frame.parent_session_id) {
            let _ = subscriber.tx.send(frame);
        }
    }

    /// Live subscribers. Diagnostics and tests only.
    pub fn subscribers(&self) -> usize {
        self.lock().len()
    }
}

/// A parent turn's end of the progress channel.
///
/// Holds a sender of its own, which is not redundancy: with only the bus's copy,
/// a turn whose entry had been replaced would see its channel close, and
/// `recv()` on a closed empty channel is **ready forever**. In the `biased`
/// select below that starves the engine branch completely — the turn would stop
/// streaming and never finish. Keeping a sender here means the channel can never
/// close while this stream lives, so an empty channel is always `Pending`.
pub struct ProgressStream {
    rx: tokio::sync::mpsc::UnboundedReceiver<ChildProgress>,
    _keepalive: tokio::sync::mpsc::UnboundedSender<ChildProgress>,
    bus: std::sync::Weak<ProgressBus>,
    session_id: String,
    id: u64,
}

impl ProgressStream {
    /// The next frame, waiting forever if there is none.
    ///
    /// Never returns — by design, because it is only ever awaited inside a
    /// `select!` against something that does. Cancel-safe:
    /// `UnboundedReceiver::recv` is, so losing the race costs nothing.
    pub async fn next(&mut self) -> ChildProgress {
        match self.rx.recv().await {
            Some(frame) => frame,
            // Unreachable while `_keepalive` is held. If it ever became
            // reachable, hanging is the safe answer and spinning is not: this
            // future is polled first in a biased select, so a ready `None`
            // would starve the engine branch.
            None => std::future::pending().await,
        }
    }
}

impl Drop for ProgressStream {
    fn drop(&mut self) {
        if let Some(bus) = self.bus.upgrade() {
            let mut subscribers = bus.lock();
            // Only if it is still OURS. A turn that ended after a newer turn of
            // the same session subscribed must not take the newer one's channel
            // with it.
            if subscribers
                .get(&self.session_id)
                .is_some_and(|current| current.id == self.id)
            {
                subscribers.remove(&self.session_id);
            }
        }
    }
}

/// What a parent's drain loop got when it asked for the next thing to happen.
#[derive(Debug)]
pub enum ParentStep<T> {
    /// A child said something. Yield it and go round again — it is not an
    /// engine event and must not be folded into the turn.
    Progress(ChildProgress),
    /// The engine produced an event.
    Engine(T),
    /// The engine's stream ended.
    EngineEnded,
}

/// Await whichever comes first: a progress frame, or the engine's next event.
///
/// # Why neither side starves
///
/// `biased` puts progress first, and that reads like a starvation hazard until
/// you ask WHEN a frame can exist. A synchronous delegation is produced
/// underneath the parent's own `delegate` tool call, and the engine yields
/// nothing until that call returns — goose awaits `ToolCallResult::result`
/// inline in its reply loop. So for the whole of a child's run the engine branch
/// is Pending, whether the child's future is polled by this very task or by the
/// MCP service's own, and the bias delays no engine event that exists. What it
/// buys is that a frame is delivered as soon as the engine is not ready, instead
/// of queueing behind however many events it has to hand — which for a
/// delegation means "after the child has finished", the no-news-for-minutes this
/// phase exists to remove.
///
/// The hot-engine case the guard drives — events always ready, frames arriving
/// anyway — cannot happen during a synchronous delegation today. It is the shape
/// P8's background tasks would produce, and pinning it now costs one test rather
/// than a rediscovery then.
///
/// # Cancel-safety, which this depends on
///
/// Losing the race drops the `Next` future, not the stream: `StreamExt::next`
/// borrows, and an `async_stream` generator's state lives in the stream itself,
/// so nothing in flight is lost and the next poll resumes it. The same is true
/// of `UnboundedReceiver::recv`. `an_engine_item_survives_a_progress_frame`
/// pins it rather than trusting the argument.
pub async fn next_parent_step<S>(
    engine: &mut S,
    progress: &mut ProgressStream,
) -> ParentStep<S::Item>
where
    S: futures::Stream + Unpin,
{
    tokio::select! {
        biased;
        frame = progress.next() => ParentStep::Progress(frame),
        item = futures::StreamExt::next(engine) => match item {
            Some(item) => ParentStep::Engine(item),
            None => ParentStep::EngineEnded,
        },
    }
}

/// Publish one frame about a live delegation, if anybody is listening.
///
/// The one entry point, so that every producer spells the routing key the same
/// way and there is one place to look for what a child is allowed to say.
pub fn report_child_progress(
    parent_session_id: &str,
    task_id: &str,
    role: &str,
    status: SubagentStatus,
    detail: Option<String>,
) {
    process_progress_bus().publish(ChildProgress {
        parent_session_id: parent_session_id.to_string(),
        task_id: task_id.to_string(),
        role: role.to_string(),
        status,
        detail,
    });
}

/// The tool names in one of a child's messages, and NOTHING else from it.
///
/// # This is PAI-5's reasoning gate, re-applied
///
/// The drain loop reduces a child's messages with `as_concat_text()`, which
/// filters on `as_text()` and therefore returns `None` for
/// `MessageContent::Thinking`. That is not incidental: PAI-5 P1 gates reasoning
/// ONCE, at the `GooseAdapter` producer, on `show_thinking && !voice_mode`, and
/// **a child does not go through that producer**. `as_concat_text()` is what has
/// been standing in for the gate on this path.
///
/// P6 has to read `msg.content` directly to see a `ToolRequest`, which loses
/// that protection. It is replaced here rather than inherited, and structurally
/// rather than by a check somebody has to remember: this function's return type
/// is a list of tool NAMES, so there is no value it could return that carries a
/// child's reasoning, its answer text, or a tool call's ARGUMENTS — which on
/// this pond can be a household memory query or the state of a device (PAI-2
/// minimisation). Nothing else in the loop touches `msg.content`.
///
/// `a_progress_frame_carries_the_tool_name_and_nothing_else` drives it with a
/// message holding all four kinds of content at once.
///
/// A tool the child asked for in a way Goose could not parse (`tool_call` is
/// `Err`) is skipped: there is no name to report, and inventing one would put a
/// model's malformed output on the wire as though it were a call.
pub fn child_tool_names(msg: &goose::conversation::message::Message) -> Vec<String> {
    use goose::conversation::message::MessageContent;
    msg.content
        .iter()
        .filter_map(|content| match content {
            MessageContent::ToolRequest(request) => request
                .tool_call
                .as_ref()
                .ok()
                .map(|call| call.name.to_string()),
            _ => None,
        })
        .collect()
}

/// Split a tool name the way Goose's `list_tools` reports it into
/// `(extension, bare tool)`, or `None` for a name that belongs to no extension.
///
/// This is the front door of [`ChildEnvironment::parent_tools`] and it is a
/// narrowing, not a parse. `PLATFORM_EXTENSIONS` marks `developer`, `summon`,
/// `analyze`, `skills` and code-mode with `unprefixed_tools: true`, and
/// `extension_manager.rs`'s `is_unprefixed_extension` then exposes their tools
/// under their BARE names — `shell`, `delegate` — with no `ext__` prefix
/// anywhere. A bare name is indistinguishable from Goose plumbing (the
/// final-output tool, platform tools), so guessing an owner for it would be
/// inventing an extension the engine never named. It is dropped instead, which
/// is why a parent whose builtin strip failed cannot leak `developer`'s tools
/// into a plan even before [`TaskSpec::grants_tool`] gets a say.
///
/// Verified against the submodule 2026-08-09.
pub fn split_extension_tool(tool_name: &str) -> Option<(&str, &str)> {
    let at = tool_name.find(TOOL_NAME_SEPARATOR)?;
    let (extension, rest) = tool_name.split_at(at);
    if extension.is_empty() {
        return None;
    }
    Some((extension, &rest[TOOL_NAME_SEPARATOR.len()..]))
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
    /// Every fully-qualified (`extension__tool`) name the child may call.
    ///
    /// PAI-6 P3. Derived from `extensions` in [`build_child_plan`] so the two
    /// cannot disagree, and published to the child session's `ShimControls`
    /// before the first provider call. That is the SECOND layer of invariant 1's
    /// tool axis: `ExtensionConfig::available_tools` is checked inside
    /// `dispatch_tool_call`, which stops a call, and this one stops the tool
    /// being LISTED to the model at all. The shim is otherwise inert for a
    /// child — `ShimControls::existing_session` returns `None` for a session
    /// GIAP never chatted in, and `enforce_tools(tools, &None)` is a no-op with
    /// no warning.
    pub allowed_tool_names: Vec<String>,
    pub max_turns: u32,
    /// PAI-6 P7. Which model this child runs on, already decided.
    ///
    /// The whole [`ChildModel`] and not an `Option<String>`, so that "the role
    /// asked for a model and this device refused it" is a value the runner and a
    /// test can both see, rather than an absence indistinguishable from "the
    /// role asked for nothing". [`ChildModel::assigned`] is the only predicate
    /// the runner may use.
    pub model: ChildModel,
}

/// What came back from one child run, before it is classified.
#[derive(Debug, Clone, Default)]
pub struct ChildOutcome {
    /// The whole text of the child's last COMPLETED assistant turn, assembled
    /// by [`ChildTurns`] — not of its last message, which on every streaming
    /// provider is a fragment of one.
    ///
    /// Text only: Goose's `as_text()` returns `None` for
    /// `MessageContent::Thinking`, so a child's reasoning cannot reach the
    /// parent through this field. PAI-5's gate lives at the `GooseAdapter`
    /// producer, which this path does not go through.
    pub last_text: Option<String>,
    /// Assistant TURNS seen — contiguous runs of assistant messages, counted by
    /// [`ChildTurns`].
    ///
    /// **This is not Goose's `turns_taken` and never was.** That counter lives
    /// inside `agent.rs`'s reply loop, is incremented once per provider
    /// request, and is not on any event this adapter can see; believing the two
    /// were the same thing is what made P2 count streaming fragments and report
    /// every successful delegation as `TurnBudgetExhausted`. What GIAP can see
    /// is the message stream, so what it counts is runs of it. The two agree on
    /// an ordinary turn and this one can only ever UNDER-count (an assistant
    /// system notification between two provider calls merges their runs), which
    /// is the safe direction: the sentinel check in [`classify_outcome`] is what
    /// actually catches an exhausted budget, and this is the second net.
    pub assistant_turns: u32,
    /// What the child agent actually had loaded when it ran. Audited against
    /// [`GOOSE_STRIPPED_BUILTINS`] after the fact, because
    /// `Agent::add_extension` is not the only way an extension can arrive.
    pub loaded_extensions: BTreeSet<String>,
}

/// Assemble a child's message stream into TURNS.
///
/// # Why this exists
///
/// P2's drain loop counted one assistant turn per `AgentEvent::Message` with
/// `role == Assistant`. Goose yields one of those **per provider chunk**, and
/// this repository documents the shapes in a table in `goose_agent.rs`: on
/// local/gguf — the Jetson headline configuration — one message per TOKEN
/// PIECE; on every openai-format HTTP provider (Ollama, DeepSeek, OpenRouter,
/// vLLM) one per streamed delta; on anthropic one per complete block. With a
/// role cap of at most twelve turns, a forty-token answer therefore tripped
/// `assistant_turns > max_turns` in [`classify_outcome`] and EVERY successful
/// delegation came back `TurnBudgetExhausted` with no result. The same
/// misreading assigned `last_text` per message, so the "answer" was the last
/// non-empty fragment — a word or two.
///
/// Goose's own loop treats the yields as fragments: `agent.rs` does
/// `last_assistant_text.push_str(&text)` for each one and clears it at the top
/// of each turn.
///
/// # The rule
///
/// A turn is a **contiguous run of assistant messages**. Anything else closes
/// it — goose returns a tool response as a `User` message, which is the real
/// boundary between one provider call and the next. Text accumulates within a
/// run with no separator, exactly as goose accumulates it; a run whose text is
/// blank (all `Thinking`, or a system notification) is counted but is not
/// eligible to be the answer; and the last non-blank completed run is what the
/// parent gets. [`finish`](Self::finish) flushes the run that was still open
/// when the stream ended, which is the ordinary case — nothing follows the
/// final answer.
///
/// Pure, and holding no engine type on purpose, so the decision can be driven
/// from a `Vec` of fragments instead of from a provider and a Goose session
/// store. That is the same reason [`classify_outcome`] and [`build_child_plan`]
/// are out here.
#[derive(Debug, Default)]
pub struct ChildTurns {
    /// The run currently being accumulated, if a run is open.
    open: Option<String>,
    turns: u32,
    last_completed: Option<String>,
}

impl ChildTurns {
    /// One assistant message, already reduced to its text with
    /// `as_concat_text()`.
    ///
    /// Empty text still opens a run: a thinking-only message is output the
    /// model produced, and dropping it here would let a tool response merge two
    /// separate turns into one.
    pub fn assistant_message(&mut self, text: &str) {
        match self.open.as_mut() {
            Some(open) => open.push_str(text),
            None => {
                self.turns = self.turns.saturating_add(1);
                self.open = Some(text.to_string());
            }
        }
    }

    /// One message whose role is not Assistant — a tool response, in practice.
    /// It ends whatever run was open.
    pub fn other_role_message(&mut self) {
        self.close_run();
    }

    fn close_run(&mut self) {
        if let Some(text) = self.open.take() {
            if !text.trim().is_empty() {
                self.last_completed = Some(text);
            }
        }
    }

    /// End of stream: flush the open run and report
    /// `(last completed answer, assistant turns)`.
    pub fn finish(mut self) -> (Option<String>, u32) {
        self.close_run();
        (self.last_completed, self.turns)
    }
}

/// The whole of the drain loop's per-event decision, out here where a test can
/// run it.
///
/// `GooseAdapter::run_child_agent` needs a real provider and a real Goose
/// session store, so nothing without an engine can reach the loop that feeds
/// [`ChildTurns`] — and the guard standing for it asserted only that certain
/// strings appeared in that loop's source. Both defects the turn-counting fix
/// closed can be reintroduced without moving one of those strings: closing the
/// run after every assistant message restores P2's semantics exactly (every
/// streamed fragment its own turn, every delegation `TurnBudgetExhausted`), and
/// accumulating the empty string instead of the message makes every child
/// answer nothing. Both were applied and the whole suite stayed green.
///
/// So the mapping lives here, driven by the `Frag` harness in
/// `orchestrator/tests.rs` from role-tagged fragments, and the tripwire over
/// the loop is left with the one thing that genuinely cannot be lifted: which
/// two expressions the loop passes.
pub fn child_stream_step(turns: &mut ChildTurns, is_assistant: bool, text: &str) {
    if is_assistant {
        turns.assistant_message(text);
    } else {
        // Goose returns a tool response as a `User` message. That is the
        // boundary between one provider call and the next, which is what a
        // turn actually is.
        turns.other_role_message();
    }
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
///
/// `persona` is the role's stored `instructions` — its standing character, as
/// distinct from `spec.instructions()`, which is what THIS child was asked to
/// do and goes in the user message. See [`build_child_plan`] for why it is an
/// `Option` today.
fn subagent_envelope(spec: &TaskSpec, persona: Option<&str>, tools: &[String]) -> String {
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
    envelope.push_str(&format!("You are the `{}` helper.\n", spec.role()));
    if let Some(persona) = persona.map(str::trim).filter(|p| !p.is_empty()) {
        envelope.push('\n');
        envelope.push_str(persona);
        envelope.push('\n');
    }
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
///
/// # `role_persona`, and why it is an `Option` that production passes `None`
///
/// A stored role's `instructions` are its persona — "you check the weather and
/// answer in one sentence". [`pond_core::shared::domain::orchestration::AgentRole`]
/// validates them as REQUIRED (there is a `MissingInstructions` error for their
/// absence) and then nothing outside a unit test reads them, so a role has been
/// functionally a name plus a tool list plus a turn budget, and the envelope
/// printed only the name.
///
/// Carrying the persona onto [`TaskSpec`] is a `pond-core` change and this
/// change does not own `pond-core`. What is implemented here is the half that
/// belongs to the adapter: the envelope renders a persona when it is given one,
/// under the heading it already had. [`GooseOrchestrator::spawn`] passes `None`
/// until `TaskSpec` can answer for it — one token at that call site, and no
/// other edit here.
pub fn build_child_plan(
    spec: &TaskSpec,
    child_session_id: &str,
    env: &ChildEnvironment,
    role_persona: Option<&str>,
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
    system_prompt.push_str(&subagent_envelope(spec, role_persona, &tool_names));

    Ok(ChildPlan {
        task_id: spec.id().to_string(),
        role: spec.role().to_string(),
        parent_session_id: spec.parent_session_id().to_string(),
        child_session_id: child_session_id.to_string(),
        system_prompt,
        user_message: child_user_message(spec),
        extensions,
        allowed_tool_names: tool_names,
        max_turns: spec.max_turns(),
        // PAI-6 P7, decided HERE because this is the one place that holds both
        // halves: the role's request, and the provider the parent is actually
        // on. `GooseAdapter::run_child_agent` reads the answer and never the
        // question -- it has no access to `spec` at all, which is what stops a
        // second reading of `requested_model()` growing next to the engine call.
        model: ChildModel::resolve(spec.requested_model(), &env.provider_name),
    })
}

/// Stamp a plan's model decision onto the parent's `ModelConfig` — PAI-6 P7.
///
/// # Why this is out here rather than inline at the engine
///
/// `GooseAdapter::run_child_agent` needs a live provider and a Goose session
/// store, so nothing without an engine can reach a decision made inside it — and
/// this programme's recorded shape for that is a source tripwire asserting that
/// certain strings are present, which catches a textual revert and nothing else.
/// Both of P7's natural regressions leave every string in place: matching on the
/// variant instead of calling [`ChildModel::assigned`] treats `RefusedOnDevice`
/// as an assignment and swaps the resident GGUF on the Orin, and carrying the
/// parent's `context_limit` budgets a different model against the parent's
/// window. Out here they are two assertions.
///
/// # Same provider, different config
///
/// There is no second provider built. `Provider::stream` takes the
/// `&ModelConfig` per call and `Agent::reply` reads it from the SESSION row that
/// `update_provider` writes, so a child session naming another model on the same
/// `Arc<dyn Provider>` is the whole mechanism — which is exactly why it is only
/// offered off-device, where "another model" is a field in a request body rather
/// than a second set of weights in the one GPU.
///
/// `context_limit` is cleared rather than carried: Goose's `update_provider`
/// backfills a `None` from the provider registry's entry for whatever model is
/// now named, so the child ends up budgeted against its own window.
pub fn child_model_config(
    model: &ChildModel,
    parent: goose_providers::model::ModelConfig,
) -> goose_providers::model::ModelConfig {
    let Some(assigned) = model.assigned() else {
        return parent;
    };
    let mut cfg = parent;
    cfg.model_name = assigned.to_string();
    cfg.context_limit = None;
    cfg
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

/// The registry lock, for callers that hold an `Arc` rather than `&self`.
///
/// `GooseOrchestrator::with_registry` delegates here so both paths take the lock
/// the same way; a background run has no `&self` to go through.
fn with_registry<T>(tasks: &Mutex<TaskRegistry>, f: impl FnOnce(&mut TaskRegistry) -> T) -> T {
    let mut guard = tasks.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
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

    /// The run holds its concurrency permit and is now actually running.
    ///
    /// The transition [`TaskStatus::Queued`] exists to express. Before it,
    /// `spawn` inserted the run as `Running` and a child waiting on the
    /// semaphore — which on an on-device provider is every child after the
    /// first — was reported `Running` by `poll` and `list`, so the one state
    /// the variant distinguishes was never constructed.
    fn start(&mut self, task_id: &str) {
        if let Some(entry) = self.tasks.get_mut(task_id) {
            entry.run.status = TaskStatus::Running;
        }
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

/// Run one authorised child to completion and record what it was — PAI-6 P2,
/// lifted out of `spawn` by P8.
///
/// It takes owned handles rather than `&self` for exactly one reason: a
/// background run outlives the `spawn` call that started it, so the future has
/// to be `'static`. A synchronous delegation awaits it inline and is unchanged
/// by the move — the same permit, the same frames, the same classification, in
/// the same order.
///
/// `_reservation` is a parameter and not a local because ownership is the whole
/// mechanism: PAI-6 P4's claim on the parent's history budget is released by
/// `Drop`, so passing it in is what makes a background child's claim last
/// exactly as long as the child does, including when the future is dropped.
#[allow(clippy::too_many_arguments)]
async fn drive_run(
    runner: Arc<dyn ChildRunner>,
    tasks: Arc<Mutex<TaskRegistry>>,
    ledger: Arc<DeviceLedger>,
    permits: Arc<Semaphore>,
    provider_name: String,
    plan: ChildPlan,
    mut run: TaskRun,
    cancel: CancellationToken,
    _reservation: HistoryReservation,
) -> Result<TaskRun> {
    let parent_session_id = run.parent_session_id.clone();
    let plan_child_session_id = plan.child_session_id.clone();
    // Invariant 3, on the only path that can start a child. The permit is
    // acquired BEFORE the engine is touched and held until the run ends, so
    // there is no window in which two on-device children are both replying.
    //
    // **Unless the parent's own turn is already holding the device**, which
    // is the ordinary case for a synchronous delegation: the parent is
    // blocked inside a tool call, not talking to the provider, so there is
    // exactly one thing using the model and the child is it. Acquiring
    // again would deadlock the parent against its own child forever, and
    // because that turn is also holding one of `main.rs`'s four
    // `sse_semaphore` permits, four of them would take the interactive chat
    // pool down until the process restarts. Inheriting is not a relaxation
    // of invariant 3 — the parent cannot reply while its child runs.
    //
    // What inheriting must NOT mean is `needed = 0`, which is what P4
    // wrote. A device hold belongs to the SESSION, so every child of the
    // delegating turn inherited it and `acquire_many_owned(0)` never
    // blocks: three delegations issued in one parent turn ran three
    // abreast on the one GPU. The parent's hold therefore carries a
    // semaphore of ONE for its children to share — a child skips its
    // parent's claim and still queues behind its siblings.
    let (device, needed) = match ledger.inherited_child_permits(&parent_session_id) {
        Some(siblings) => (siblings, 1),
        None => (permits, subagent_permits(&provider_name)),
    };
    let permit = tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        acquired = device.acquire_many_owned(needed) => Some(acquired?),
    };
    let Some(_permit) = permit else {
        // Cancelled while queued. Nothing ran, so there is nothing to
        // classify -- but the status still has to say Cancelled rather than
        // look like a completion with no result.
        runner.release(&plan_child_session_id).await;
        with_registry(&tasks, |registry| {
            registry.finish(&run.id, TaskStatus::Cancelled, None, None)
        });
        run.status = TaskStatus::Cancelled;
        run.finished_at = Some(chrono::Utc::now());
        report_child_progress(
            &parent_session_id,
            &run.id,
            &run.role,
            SubagentStatus::Cancelled,
            None,
        );
        return Ok(run);
    };

    // The permit is held from here to the end of the run, so this is the
    // moment the run stops waiting and starts.
    run.status = TaskStatus::Running;
    with_registry(&tasks, |registry| registry.start(&run.id));
    report_child_progress(
        &parent_session_id,
        &run.id,
        &run.role,
        SubagentStatus::Running,
        None,
    );

    let max_turns = plan.max_turns;
    let outcome = runner.run(plan, cancel.clone()).await;
    runner.release(&plan_child_session_id).await;

    let (status, result, error) = match outcome {
        Ok(outcome) => {
            // Invariant 2, audited after the fact. The plan builder refuses
            // to ASK for a stripped builtin; this asks what the child
            // actually ended up holding, because `add_extension` is not the
            // only way one can arrive (a Goose sync could re-arm a
            // `default_enabled` platform extension, which is exactly what
            // `EnabledExtensionsState::extensions_or_default` does).
            //
            // `run_child_agent` reads `list_extensions()` AFTER the drain
            // and unions it with the pre-run read, so this set is what the
            // child ran with rather than an echo of what it was handed. P2
            // read it before `reply` and could therefore only ever report
            // back what `add_extension` had just been given -- which
            // `child_extensions` had already refused, so the audit was
            // structurally incapable of failing.
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

    with_registry(&tasks, |registry| {
        registry.finish(&run.id, status, result.clone(), error.clone())
    });
    // The terminal frame. `error` and not `result`: a run's error is a
    // sentence GIAP wrote about a failure, and its result is the child's own
    // answer — which reaches the parent as the `delegate` tool's result and
    // has no business also arriving as a progress frame. PAI-6 invariant 4.
    report_child_progress(
        &parent_session_id,
        &run.id,
        &run.role,
        SubagentStatus::from(status),
        error.clone(),
    );
    run.status = status;
    run.result = result;
    run.error = error;
    run.finished_at = Some(chrono::Utc::now());
    Ok(run)
}

// ── The orchestrator ────────────────────────────────────────────────────────

/// Adapter: runs [`TaskSpec`]s as Goose child agents.
pub struct GooseOrchestrator {
    runner: Arc<dyn ChildRunner>,
    /// One semaphore for every child on this pond. Invariant 3 is a property of
    /// the device, not of a session, so the limit is process-wide — see
    /// [`process_subagent_permits`], which is the only place this is obtained
    /// from.
    permits: Arc<Semaphore>,
    /// Behind an `Arc` since PAI-6 P8: a background run outlives the `&self`
    /// that started it, so the part of this struct it keeps using has to be
    /// something it can own a handle to. Nothing else about it changed.
    tasks: Arc<Mutex<TaskRegistry>>,
    /// The same registry `GooseAdapter` publishes each turn's authority into.
    ///
    /// PAI-6 P3. Two things depend on it, and both are refusals rather than
    /// conveniences: a spec whose parent turn is no longer live does not run at
    /// all, and a child that does run gets a token DERIVED from the parent's, so
    /// cancelling the parent cancels its children (invariant 5's other half).
    authorities: Arc<TurnAuthorityRegistry>,
    /// PAI-6 P4. Written here, read by `GooseAdapter::turn_profile`. Also
    /// process-wide, and for the same reason as `permits` — see
    /// [`process_device_ledger`].
    ledger: Arc<DeviceLedger>,
}

impl GooseOrchestrator {
    pub fn new(runner: Arc<dyn ChildRunner>, authorities: Arc<TurnAuthorityRegistry>) -> Self {
        Self {
            runner,
            permits: process_subagent_permits(),
            tasks: Arc::new(Mutex::new(TaskRegistry::default())),
            authorities,
            ledger: process_device_ledger(),
        }
    }

    fn with_registry<T>(&self, f: impl FnOnce(&mut TaskRegistry) -> T) -> T {
        with_registry(&self.tasks, f)
    }
}

#[async_trait]
impl Orchestrator for GooseOrchestrator {
    async fn spawn(&self, spec: TaskSpec) -> Result<TaskRun> {
        // PAI-6 P3, and the first thing checked because it is the cheapest
        // refusal. A `TaskSpec` was authorised by a parent turn; if that turn is
        // over, the authority behind it is stale and the only safe answer is no.
        // This is also what makes the registry load-bearing rather than
        // decorative: there is no `spawn` path that runs without a live parent.
        //
        // For a SYNCHRONOUS run the token is DERIVED from the parent's rather
        // than minted fresh, which is invariant 5's other half. Goose's own
        // background path mints an unrelated root token and relies on a `Drop`
        // sweep at shutdown; `child_token()` is unused anywhere in the engine.
        //
        // PAI-6 P8 splits that in two, and the split is a decision rather than
        // an omission. A background run outlives the turn that asked for it by
        // definition, and the parent turn's token is cancelled by the
        // `DropGuard` the chat stream holds — so a background child that
        // inherited would die the moment its parent's reply finished, which is
        // not a background child, it is a synchronous one with a race. It
        // therefore gets its OWN token, whose owner is the parent SESSION, and
        // the cascade for it is `cancel_children_of` (plus `cancel` by id).
        // Both are on the port and both are implemented below; what invariant 5
        // is owed and has not got is a PRODUCTION caller for the first, on
        // session deletion and on shutdown, which lives outside this crate.
        let parent_turn = self
            .authorities
            .parent_turn_token(spec.parent_session_id())
            .ok_or_else(|| {
                anyhow!(
                    "no live turn holds the authority for session `{}` - refusing to run a \
                     delegation whose parent has already ended",
                    spec.parent_session_id()
                )
            })?;

        // PAI-6 P4. The child's claim on its parent's history budget, taken the
        // moment the delegation is AUTHORISED rather than when it starts
        // replying. A child the parent has committed to is a second claim on
        // the window whether it is queueing for the device, being planned, or
        // talking; shrinking the parent's budget a little early costs recall it
        // was going to lose anyway, and shrinking it late means a concurrent
        // parent turn trims as though it still owned the whole window.
        //
        // Held to the end of `spawn` by `Drop`, which is what makes every exit
        // below correct without any of them saying so — a refused plan, a
        // runner that errored, a cancellation while queued and an ordinary
        // completion all give the budget back on the way out. It is deliberately
        // ABOVE the plan builder: a first version sat below it, so the refusal
        // path never held a reservation at all and the test asserting that the
        // refusal releases one passed without ever exercising a release.
        let reservation = self
            .ledger
            .reserve(spec.parent_session_id(), spec.context_fraction());

        let env = self.runner.environment(spec.parent_session_id()).await?;

        // PAI-6 P8, refused here: after the provider is known and BEFORE the
        // engine is touched, so a refused background call leaves no child
        // session behind to release.
        //
        // The refusal is invariant 3 in different clothes. Where only one agent
        // may hold the model at a time, a background child does not run beside
        // the conversation — it runs INSTEAD of it, and the parent's next turn
        // waits for it and then pays a re-prefill. Silently running it in the
        // foreground was the alternative and is worse: the caller was told its
        // turn would return immediately, and it would not.
        if spec.background() {
            if let Some(refusal) =
                BackgroundAvailability::for_provider(&env.provider_name).refusal()
            {
                tracing::info!(
                    target: "giap::trace",
                    kind = "background_delegation_refused",
                    role = %spec.role(),
                    provider = %env.provider_name,
                    "a background delegation was refused"
                );
                return Err(anyhow!(refusal));
            }
        }

        // Only now is the token settled, because only now is it known whether
        // this run outlives its parent's turn.
        let cancel = if spec.background() {
            CancellationToken::new()
        } else {
            parent_turn.child_token()
        };

        let child_session_id = self.runner.open_child_session(spec.role()).await?;
        // `None`: the role's persona is not on `TaskSpec` yet. See
        // `build_child_plan`'s own doc for the pond-core accessor this becomes.
        let role_persona: Option<&str> = None;
        let plan = match build_child_plan(&spec, &child_session_id, &env, role_persona) {
            Ok(plan) => plan,
            Err(refused) => {
                self.runner.release(&child_session_id).await;
                return Err(anyhow!(refused));
            }
        };

        // PAI-6 P7, traced from one place so the two outcomes are read together.
        //
        // The refusal is not silent: it is a role's stated intent going unmet,
        // and the person who authored the role is the only one who can decide
        // what to do about it. It is a WARN rather than an error because the
        // delegation itself is fine -- the child runs, on the resident model,
        // with the tools, the budget and the persona the role asked for, all of
        // which is where a role's value actually is.
        match (plan.model.assigned(), plan.model.refusal()) {
            (Some(model), _) => tracing::info!(
                target: "giap::trace",
                kind = "role_model_assigned",
                task_id = %plan.task_id,
                role = %plan.role,
                model = %model,
                "subagent runs on the model its role asked for"
            ),
            (None, Some(refusal)) => tracing::warn!(
                target: "giap::trace",
                kind = "role_model_refused",
                task_id = %plan.task_id,
                role = %plan.role,
                "{refusal}"
            ),
            (None, None) => {}
        }

        // Queued, not Running: the permit is acquired below and on an on-device
        // provider every child after the first waits for it. `TaskRun::started`
        // is the only constructor `pond-core` offers and it stamps `Running`, so
        // the status is corrected here rather than by building the struct field
        // by field — a `TaskRun::queued` constructor over there would say it
        // once instead of twice, and is requested rather than written because
        // this change does not own `pond-core`.
        let mut run = TaskRun::started(&spec, chrono::Utc::now());
        run.status = TaskStatus::Queued;
        self.with_registry(|registry| registry.insert(run.clone(), cancel.clone()));

        // PAI-6 P6. The first frame the parent's stream can draw a node from,
        // and it is emitted BEFORE the permit is acquired on purpose: on-device
        // that acquisition is where a second delegation from the same turn
        // spends its time, and a child that is queueing is the state most in
        // need of saying so. Every frame in this method is derived from the
        // registry's own status, so a run that reaches a state the stream never
        // hears about is a missing `report_child_progress` next to a
        // `registry` call rather than a second lifecycle to keep in step.
        report_child_progress(
            spec.parent_session_id(),
            &run.id,
            &run.role,
            SubagentStatus::Queued,
            None,
        );

        // PAI-6 P8. The fork, and it is deliberately the LAST thing: everything
        // above — the live-turn check, the background refusal, the reservation,
        // the plan and its refusals, the registry entry and the first frame —
        // happens for both kinds of run, synchronously, so a background call
        // that cannot proceed says so to the caller's face instead of failing
        // somewhere nobody is listening.
        //
        // What differs after this line is only who awaits. `Orchestrator::poll`
        // existed before there was anything to poll precisely so this could be
        // added without touching the port, and `TaskStatus` already had the
        // terminal states, so there is no second lifecycle here.
        let drive = drive_run(
            self.runner.clone(),
            self.tasks.clone(),
            self.ledger.clone(),
            self.permits.clone(),
            env.provider_name.clone(),
            plan,
            run.clone(),
            cancel,
            reservation,
        );
        if spec.background() {
            tokio::spawn(async move {
                if let Err(e) = drive.await {
                    // `drive_run` only errors before it can record anything, so
                    // there is nobody left to tell but the log. A synchronous
                    // caller gets this as the `delegate` tool's answer; a
                    // background one has already been told its task id.
                    tracing::error!(
                        target: "giap::trace",
                        kind = "background_delegation_failed",
                        "a background delegation could not be driven: {e}"
                    );
                }
            });
            // `Queued`, which is what it is: the run has an identity and has not
            // got a permit. The port's own contract for a background
            // implementation, and `describe_run` already reads it as "still
            // working".
            return Ok(run);
        }
        drive.await
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

    /// PAI-6 invariant 5's other half, and since P8 it is the ONLY half for a
    /// background run.
    ///
    /// A synchronous child holds a token derived from its parent turn's, so the
    /// chat stream's `DropGuard` already reaches it and this method is for the
    /// explicit cases. A BACKGROUND child holds its own — it has to, or it would
    /// die with the turn that asked for it — so this is what stands between a
    /// deleted session and a helper still working on its behalf.
    ///
    /// **It still has no production caller, and P8 did not add one**: session
    /// deletion and shutdown live in `pond-api`'s routes and `pond-server`'s
    /// `main.rs`. That is stated here rather than implied by the absence,
    /// because the absence is exactly what a reader would otherwise take for
    /// "nothing needs to call it".
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
