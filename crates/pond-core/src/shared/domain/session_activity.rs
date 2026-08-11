//! Session-activity lifecycle on the reactive event spine (PAI-7 P1).
//!
//! Tells a later proactive phase whether the user is *available*: the
//! difference between a helpful nudge and an interruption
//! (`docs/architecture/pai/07-proactive-intelligence.md` section 3.1).
//!
//! Two sources feed it, and neither is new -- both already carry the pond's
//! notion of "the user is here":
//!
//! - **The session store**, whose `created_at` says a conversation began.
//! - **The activity clock**, `last_user_activity` plus the newest
//!   `sessions.updated_at`, combined by
//!   [`crate::user_data::services::consolidation_schedule::combined_idle_for`]
//!   so an out-of-process voice turn counts as activity too.
//!
//! **Both sources are filtered through [`SessionOrigin`] first.** The pond
//! creates conversations for its own background work -- a cron line firing
//! `AgentPrompt` at 3am mints `sched-{task}-{ts}` and calls `create_session`
//! -- and those rows are byte-identical in shape to a person's. Reading them
//! as presence tells a proposer somebody is home when the house is empty,
//! which is worse than publishing no presence signal at all, because the
//! reviewer that consumes it will act on it with confidence.
//!
//! Idle is defined by the same threshold background consolidation uses. One
//! definition of "the user has gone quiet" per pond, not two: the phase that
//! will consume these events is gated by that module's `should_run`, and a
//! second, disagreeing threshold would mean the bus says the user left while
//! the gate says they are still here.
//!
//! Pure domain: [`ActivityObserver::poll`] is a function of the session rows
//! and the clock reading it is handed plus its own recorded phase, so every
//! transition -- and every one of the wiring decisions that used to live in
//! `pond-server`'s polling loop -- is unit-testable without a clock, a
//! database, or a bus.
//!
//! # Presence (PAI-7 P2)
//!
//! [`PresenceObserver`] lives here rather than in a module of its own because
//! it is the *same observation*: the same poll of the same store, the same
//! [`SessionOrigin`] filter, and the same idle threshold deciding when the
//! evidence has gone stale. Splitting it would have created the one thing
//! `human_activity` was written to prevent -- two call sites where one gets
//! fixed and the other does not.
//!
//! The difference is what it answers. Session lifecycle says *somebody* is
//! here; presence says *who*, and it only speaks when it can name a household
//! member ([PAI-7](../../../../../docs/architecture/pai/07-proactive-intelligence.md)
//! invariants 4 and 5). Naming is delegated wholesale to PAI-1's
//! [`identity_resolution::resolve`], so presence and authorisation cannot
//! disagree about whose turn it is.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::session::{IdentificationSource, Session, SessionIdentity};
use crate::user_data::services::consolidation_schedule as sched;
use crate::user_data::services::identity_resolution;

/// Where the user is in an interaction, as far as the pond can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    /// A conversation began.
    Started,
    /// The pond has been quiet for the idle threshold, having been active.
    Idle,
    /// Activity reappeared after an [`Idle`](SessionPhase::Idle).
    Resumed,
}

impl SessionPhase {
    /// Short, stable label for structured logs and the event log.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionPhase::Started => "started",
            SessionPhase::Idle => "idle",
            SessionPhase::Resumed => "resumed",
        }
    }
}

/// Prefixes the pond puts on session ids it mints for **its own** background
/// work.
///
/// One entry, and it is the scheduler's: `AgentScheduleExecutor::run_agent_prompt`
/// (`pond-server/src/schedule_executors.rs`) builds `sched-{task_id}-{unix_ts}`
/// for every `AgentPrompt` schedule fire and every `TriggerAction::AgentPrompt`
/// a sensor rule performs, then calls `create_session` on it.
///
/// **A deny-list, not an allow-list of human shapes**, because there is no
/// human shape to require: a person's session id is whatever client opened the
/// conversation chose -- the dashboard's UUID (`routes.rs`), the CLI's
/// `--session-id`, the voice child's. Requiring a shape would silently drop the
/// voice turns that are the main presence signal on a Jetson.
///
/// A deny-list is only as complete as the audit behind it, so the audit is
/// itself a test: `pond-core/tests/session_origin_covers_every_minted_session.rs`
/// fails when a source file that mints a session row is not one of the files
/// this list was written against.
pub const POND_AUTHORED_SESSION_PREFIXES: &[&str] = &["sched-"];

/// Who caused a session row to exist.
///
/// The `sessions` table has no origin column and PAI-7 P1 may not add one (the
/// schema lives in `pond-infra`), so this is decided from the id. See
/// [`POND_AUTHORED_SESSION_PREFIXES`] for why the predicate is shaped as a
/// deny-list and what keeps it complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrigin {
    /// Somebody opened this conversation.
    Human,
    /// The pond opened it for itself, to run background work.
    Machine,
}

impl SessionOrigin {
    /// Classify a session id.
    pub fn of(session_id: &str) -> Self {
        if POND_AUTHORED_SESSION_PREFIXES
            .iter()
            .any(|prefix| session_id.starts_with(prefix))
        {
            SessionOrigin::Machine
        } else {
            SessionOrigin::Human
        }
    }

    /// True when this session is somebody's conversation.
    pub fn is_human(self) -> bool {
        matches!(self, SessionOrigin::Human)
    }
}

/// A transition in the user's interaction with the pond.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionLifecycle {
    pub phase: SessionPhase,
    /// The conversation this transition is attributed to, when one is known.
    ///
    /// [`Started`](SessionPhase::Started) always carries an id.
    /// [`Idle`](SessionPhase::Idle) and [`Resumed`](SessionPhase::Resumed)
    /// describe the pond's activity clock, which is not per-session, and carry
    /// `None` rather than guessing at the most recent conversation -- a guess
    /// there would attribute one household member's silence to another
    /// member's session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub at: DateTime<Utc>,
    /// How long the pond had been quiet immediately before this transition.
    pub idle_secs: u64,
}

/// A conversation the pond knows about, projected onto what the observer needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStart<'a> {
    pub id: &'a str,
    pub created_at: DateTime<Utc>,
    /// Carried rather than re-derived so a list handed to the observer says
    /// what it contains. [`ActivityObserver`] refuses to announce anything but
    /// [`SessionOrigin::Human`], so this is the second gate behind
    /// [`human_activity`]'s filter and not a hint.
    pub origin: SessionOrigin,
}

impl<'a> SessionStart<'a> {
    /// Project one stored session.
    pub fn of(session: &'a Session) -> Self {
        Self {
            id: &session.id,
            created_at: session.created_at,
            origin: SessionOrigin::of(&session.id),
        }
    }
}

/// The session store as the observer is allowed to see it: the pond's own
/// conversations removed from **both** halves.
///
/// One function returning both projections on purpose. Filtering the arrival
/// list while leaving the activity clock unfiltered would still let a 3am cron
/// fire open the never-at-startup gate, and two call sites is exactly how one
/// of them gets fixed and the other does not.
#[derive(Debug, Clone, PartialEq)]
pub struct HumanActivity<'a> {
    /// Conversations a person opened, in store order.
    pub starts: Vec<SessionStart<'a>>,
    /// Newest `updated_at` among those conversations -- the out-of-process
    /// activity source (a voice turn persisted by another process).
    pub newest_activity: Option<DateTime<Utc>>,
}

/// Project the session store onto the conversations a person had.
pub fn human_activity(sessions: &[Session]) -> HumanActivity<'_> {
    let mut starts = Vec::new();
    let mut newest_activity: Option<DateTime<Utc>> = None;
    for session in sessions {
        if !SessionOrigin::of(&session.id).is_human() {
            continue;
        }
        newest_activity = newest_activity.max(Some(session.updated_at));
        starts.push(SessionStart::of(session));
    }
    HumanActivity {
        starts,
        newest_activity,
    }
}

/// Everything one observation needs from outside.
#[derive(Debug, Clone, Copy)]
pub struct ActivityInputs {
    /// Whether real user activity has been observed since this process
    /// started, from
    /// [`saw_activity_since_start`](crate::user_data::services::consolidation_schedule::saw_activity_since_start).
    ///
    /// Until it is true the activity clock still holds its boot value, which
    /// is indistinguishable from a genuinely idle user -- the exact confusion
    /// that made background consolidation fire on untouched machines. An
    /// `Idle` event published from it would tell a proposer the user has gone
    /// away when in truth nobody has ever arrived.
    pub saw_activity_since_start: bool,
    /// How long since the most recent activity from any source.
    pub idle_for: Duration,
    /// How long the pond must be quiet before the user counts as away.
    pub idle_threshold: Duration,
    pub now: DateTime<Utc>,
}

/// One reading of the clocks the polling loop owns.
///
/// This is the whole of what `pond-server`'s observer task decides per poll:
/// it reads these five values and hands them over. The gate, the idle
/// arithmetic and the machine-session filter all happen in [`poll_inputs`],
/// where they are tested, rather than at a call site inside a timer loop where
/// nothing can see them.
#[derive(Debug, Clone, Copy)]
pub struct PollClock {
    /// `Instant` captured during startup wiring, before any request could be
    /// served. The baseline the never-at-startup guard measures against.
    pub started_at: Instant,
    /// The same moment in UTC, for comparing against database timestamps.
    pub started_at_utc: DateTime<Utc>,
    /// The shared `last_user_activity` clock, bumped by every HTTP route.
    pub in_process_at: Instant,
    pub now: DateTime<Utc>,
    /// How long the pond must be quiet before the user counts as away
    /// (`INACTIVITY_THRESHOLD_SECS`).
    pub idle_threshold: Duration,
}

/// What one poll of the session store means, before any transition is derived.
///
/// Public so the never-at-startup gate is assertable directly: whether a
/// scheduled run counts as the user being here is the single most consequential
/// bit on this path, and observing it only through the events it eventually
/// produces makes the test depend on a wall clock it cannot move.
pub fn poll_inputs(sessions: &[Session], clock: PollClock) -> ActivityInputs {
    let db_activity = human_activity(sessions).newest_activity;
    ActivityInputs {
        saw_activity_since_start: sched::saw_activity_since_start(
            clock.started_at,
            clock.in_process_at,
            clock.started_at_utc,
            db_activity,
        ),
        idle_for: sched::combined_idle_for(clock.in_process_at, db_activity, clock.now),
        idle_threshold: clock.idle_threshold,
        now: clock.now,
    }
}

/// What the observer last saw. Not published: it is the baseline transitions
/// are measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Observed {
    Active,
    Idle,
}

/// Turns the pond's session list and activity clock into
/// [`SessionLifecycle`] transitions.
///
/// Owned by the publisher task, which polls; every decision it makes is in
/// [`poll`](ActivityObserver::poll).
#[derive(Debug, Clone)]
pub struct ActivityObserver {
    phase: Option<Observed>,
    /// Every conversation already accounted for: the ones that existed when
    /// the baseline was taken, plus every one announced since. `None` means no
    /// baseline has been taken yet.
    ///
    /// **Ids, not a timestamp watermark.** The store's timestamps come from
    /// SQLite's `datetime('now')`, which has no fractional part, so a
    /// watermark comparison drops any conversation that begins in the same
    /// wall-clock second as the previous one -- silently, and forever. Ids
    /// also make announcement idempotent: a row whose `created_at` cannot be
    /// parsed reads as *now* on every poll (`sqlite_session_storage.rs ::
    /// parse_dt` invents `Utc::now()` rather than failing), and against a
    /// watermark that is an unbounded stream of "the user just arrived".
    ///
    /// Not pruned when a session is deleted: the store is read whole on every
    /// poll, so this set is the same order of memory as one poll already
    /// costs, and forgetting an id is the direction that re-announces.
    known: Option<HashSet<String>>,
}

impl ActivityObserver {
    /// Begin observing a pond whose existing conversations are `sessions`.
    ///
    /// Seeding is what stops a restart announcing the pond's entire chat
    /// history as newly started. **A failed read is not an empty pond**: it is
    /// "I do not know what is there", and the honest response is to take the
    /// baseline from the first poll that does succeed
    /// ([`awaiting_baseline`](ActivityObserver::awaiting_baseline)) rather than
    /// to announce a hundred conversations that were already there.
    pub fn seeded_from<E>(sessions: Result<Vec<Session>, E>) -> Self {
        match sessions {
            Ok(sessions) => Self {
                phase: None,
                known: Some(sessions.into_iter().map(|s| s.id).collect()),
            },
            Err(_) => Self::awaiting_baseline(),
        }
    }

    /// Begin observing without knowing what the pond already holds. The first
    /// poll records what it finds and announces none of it.
    pub fn awaiting_baseline() -> Self {
        Self {
            phase: None,
            known: None,
        }
    }

    /// Fold one poll of the session store in and return the transitions it
    /// produced, in the order they happened.
    ///
    /// Usually empty: most polls see the same phase as the last one. A poll
    /// can produce several `Started` events when several conversations began
    /// inside one polling interval, and at most one activity-clock transition.
    pub fn poll(&mut self, sessions: &[Session], clock: PollClock) -> Vec<SessionLifecycle> {
        let inputs = poll_inputs(sessions, clock);
        self.observe(&human_activity(sessions).starts, inputs)
    }

    /// The transition rules, over inputs that have already been projected.
    ///
    /// Private: [`poll`](ActivityObserver::poll) is the only way in, so the
    /// machine-session filter and the never-at-startup gate cannot be routed
    /// around by a caller assembling its own inputs.
    fn observe(
        &mut self,
        sessions: &[SessionStart<'_>],
        inputs: ActivityInputs,
    ) -> Vec<SessionLifecycle> {
        let mut out = Vec::new();

        // ── New conversations ────────────────────────────────────────────
        // A *person's* session row appearing is itself evidence of activity,
        // so this half is not gated on `saw_activity_since_start`: the only
        // production paths that create a row with an id outside
        // `POND_AUTHORED_SESSION_PREFIXES` are an HTTP request and the voice
        // CLI, and both mean somebody is at the pond. A pond-authored row
        // means only that a cron line fired, so it is filtered out here as
        // well as in `human_activity` -- one gate at the projection and one at
        // the decision, because this is the claim P4 will interrupt a person
        // on.
        let taking_baseline = self.known.is_none();
        let known = self.known.get_or_insert_with(HashSet::new);
        let mut fresh: Vec<&SessionStart<'_>> = sessions
            .iter()
            .filter(|s| s.origin.is_human() && !known.contains(s.id))
            .collect();
        fresh.sort_by_key(|s| s.created_at);
        for session in &fresh {
            known.insert(session.id.to_string());
        }
        if !taking_baseline {
            for session in fresh {
                out.push(SessionLifecycle {
                    phase: SessionPhase::Started,
                    session_id: Some(session.id.to_string()),
                    at: inputs.now,
                    idle_secs: inputs.idle_for.as_secs(),
                });
            }
        }
        if !out.is_empty() {
            // Starting a conversation is arriving. Recording it here is what
            // keeps the clock below from saying `Resumed` about the same
            // arrival one line later: one event per thing that happened.
            self.phase = Some(Observed::Active);
        }

        // ── The activity clock ───────────────────────────────────────────
        if !inputs.saw_activity_since_start {
            return out;
        }
        let quiet = inputs.idle_for >= inputs.idle_threshold;
        match (self.phase, quiet) {
            // First observation of a pond that has seen real activity: record
            // the baseline, publish nothing. There is no transition yet, and
            // inventing one would fire on every restart.
            (None, _) => {
                self.phase = Some(if quiet {
                    Observed::Idle
                } else {
                    Observed::Active
                });
            }
            (Some(Observed::Active), true) => {
                self.phase = Some(Observed::Idle);
                out.push(SessionLifecycle {
                    phase: SessionPhase::Idle,
                    session_id: None,
                    at: inputs.now,
                    idle_secs: inputs.idle_for.as_secs(),
                });
            }
            (Some(Observed::Idle), false) => {
                self.phase = Some(Observed::Active);
                out.push(SessionLifecycle {
                    phase: SessionPhase::Resumed,
                    session_id: None,
                    at: inputs.now,
                    idle_secs: inputs.idle_for.as_secs(),
                });
            }
            _ => {}
        }
        out
    }
}

// ── Presence (PAI-7 P2) ──────────────────────────────────────────────────

/// Which way a household member's presence changed.
///
/// Edges, not levels. "Jerry is here" is a level and the pond re-derives it on
/// every poll; a proposer must not be told it four hundred times a day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceTransition {
    /// The pond gained fresh evidence naming this member, having had none.
    Arrived,
    /// The evidence the pond was holding went stale, or was released.
    ///
    /// **Nothing observes somebody leaving.** There is no departure signal in
    /// this house: no geofence, no door sensor bound to a person, no camera
    /// that reports an empty room. So absence is *decided*, by the evidence
    /// ageing past [`PresenceInputs::presence_window`], and the honest reading
    /// of this variant is "the pond stopped being able to say this member is
    /// here" rather than "this member walked out".
    Departed,
}

impl PresenceTransition {
    /// Short, stable label for structured logs and the event log.
    pub fn as_str(self) -> &'static str {
        match self {
            PresenceTransition::Arrived => "arrived",
            PresenceTransition::Departed => "departed",
        }
    }
}

/// A household member arrived or left, as far as the pond can tell.
///
/// **[`profile_id`](Self::profile_id) is a `String` and not an `Option`, and
/// there is no anonymous variant.** That is invariant 4 made structural: a
/// presence event that cannot name a member is not a presence event, it is a
/// motion sensor, and this type cannot express one. The observer's only exit
/// with a member's name is [`ProfileScope::Owner`] -- `Household` and `Guest`
/// both leave through the same door as an unattributed session, which is
/// invariant 5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfilePresence {
    pub profile_id: String,
    pub transition: PresenceTransition,
    /// The rung of PAI-1's chain the belief rests on.
    ///
    /// Carried because a proposer must be able to weigh it. "Liz is home
    /// because her paired phone signed a request" and "Liz is home because a
    /// camera frame matched at 0.61" are different claims, and the second is
    /// the one a photograph can make.
    pub source: IdentificationSource,
    /// Set only for [`IdentificationSource::Face`], straight off the session
    /// row. A second threshold here would be a second definition of a good
    /// match; the per-profile one at the identification edge is the definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// The conversation the belief rests on -- for [`Arrived`] the one that
    /// named them, for [`Departed`] the one that went quiet.
    ///
    /// [`Arrived`]: PresenceTransition::Arrived
    /// [`Departed`]: PresenceTransition::Departed
    pub session_id: String,
    pub at: DateTime<Utc>,
}

/// One conversation, as the presence observer is allowed to see it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresenceEvidence<'a> {
    pub session_id: &'a str,
    /// Carried rather than re-derived, for the same reason as
    /// [`SessionStart::origin`]: a list handed to an observer should say what
    /// it contains.
    pub origin: SessionOrigin,
    /// When somebody last *said* something here (`sessions.updated_at`).
    ///
    /// **Not when the attribution was written**, and the difference is the
    /// whole freshness rule. `set_session_identity` deliberately does not touch
    /// `updated_at` (it would reorder the user's history because a camera
    /// recognised somebody), so binding a face to a conversation that has been
    /// quiet for three hours leaves this timestamp three hours old and produces
    /// no presence at all. That is what stops a photograph uploaded to an old
    /// session reading as a person in the room.
    pub last_activity: DateTime<Utc>,
    /// What the session row says about who is speaking, from
    /// [`SessionStorage::get_session_identity`].
    ///
    /// [`SessionStorage::get_session_identity`]: crate::user_data::ports::session_storage::SessionStorage::get_session_identity
    pub identity: &'a SessionIdentity,
}

/// The column presence treats as the activity clock.
///
/// One function, because two call sites now need the answer -- the projection
/// below and [`attribution_candidates`] -- and the choice between the two
/// columns is the phase's headline safety property. `sessions.created_at` says
/// when a conversation *began*; `updated_at` says when somebody last *spoke*
/// in it, which is the only one of the two that is about a person.
///
/// `set_session_identity` deliberately leaves `updated_at` alone (bumping it
/// would reorder the user's history because a camera recognised somebody), so
/// binding a face to a conversation quiet for three hours leaves this three
/// hours old and produces no presence at all. That is what stops a photograph
/// uploaded to an old session reading as somebody in the room.
fn last_spoken_at(session: &Session) -> DateTime<Utc> {
    session.updated_at
}

/// Whether evidence this old still counts as somebody being here.
///
/// Named for the same reason as [`last_spoken_at`]: the comparison is shared
/// by [`present_members`], which refuses, and [`attribution_candidates`], which
/// skips a read. Two spellings of "recently" would be two definitions of it.
///
/// Closed at the far end -- evidence exactly as old as the window is stale --
/// because the pond publishes [`SessionPhase::Idle`] at that same instant, and
/// the two must not disagree.
fn is_fresh(last_activity: DateTime<Utc>, presence_window: Duration, now: DateTime<Utc>) -> bool {
    let window = i64::try_from(presence_window.as_secs()).unwrap_or(i64::MAX);
    now.signed_duration_since(last_activity).num_seconds() < window
}

impl<'a> PresenceEvidence<'a> {
    /// Project one stored session and its identity.
    ///
    /// Which column is the activity clock and which is not is a domain
    /// decision, so it is made in [`last_spoken_at`] rather than at the
    /// polling loop.
    pub fn of(session: &'a Session, identity: &'a SessionIdentity) -> Self {
        Self {
            session_id: &session.id,
            origin: SessionOrigin::of(&session.id),
            last_activity: last_spoken_at(session),
            identity,
        }
    }
}

/// How stale a conversation may be before the member it names stops counting
/// as here.
///
/// The same `INACTIVITY_THRESHOLD_SECS` that decides [`SessionPhase::Idle`].
/// One pond, one definition of "gone quiet": two would have the bus saying a
/// member is still here after it had already said the pond went idle.
///
/// **Bound here rather than at the publisher.** It was a field the polling
/// loop filled in, which meant the freshness rule the whole phase rests on
/// could be set to twenty-four hours in `main.rs` with the workspace green.
pub const PRESENCE_WINDOW: Duration = Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS);

/// The conversations whose attribution is worth a point read.
///
/// The publisher issues one `get_session_identity` per row it hands to the
/// observer, and `list_sessions` has no `LIMIT` and no time bound -- it returns
/// every conversation the pond has ever held. Three of the observer's refusals
/// need no identity at all, so the read is skipped for the rows they would
/// discard: a conversation the pond opened for itself, one nobody has spoken in
/// inside [`PRESENCE_WINDOW`], and one whose `sessions.profile_id` is NULL
/// (`get_session_identity` reads that same column, so it would answer with no
/// profile, which resolves to `Household` or `Guest` and publishes nothing
/// either way).
///
/// **Read-avoidance, not a gate.** [`present_members`] still applies origin and
/// freshness to whatever it is handed, through the same [`is_fresh`], so a
/// caller that ignores this function gets identical events for more money. That
/// is what `skipping_a_read_never_changes_who_is_published` pins.
pub fn attribution_candidates(sessions: &[Session], now: DateTime<Utc>) -> Vec<&Session> {
    sessions
        .iter()
        .filter(|session| SessionOrigin::of(&session.id).is_human())
        .filter(|session| session.profile_id.is_some())
        .filter(|session| is_fresh(last_spoken_at(session), PRESENCE_WINDOW, now))
        .collect()
}

/// Whether this pond has more than one household member, from the read that
/// answers it.
///
/// **A failed read answers `true`.** That is the value which makes an
/// unidentified speaker a `Guest` rather than the whole household: on failure,
/// access narrows. The publisher used to decide this inside its own `match`
/// arm, where the direction could be flipped with nothing to notice.
///
/// Stated plainly, because a guard that claims more than it holds is worse than
/// none: **this cannot change a presence event today.** `identity_resolution`
/// consults it only in the fallback that answers `Household` or `Guest`, and
/// presence publishes for neither, so both values produce the same events. It
/// is pinned anyway because the direction is the resolver's contract and
/// because the day presence grows a `Household` path is not the day to
/// rediscover it -- see
/// `the_household_count_cannot_change_a_published_presence_event`.
pub fn household_has_multiple_members<T, E>(profiles: &Result<Vec<T>, E>) -> bool {
    match profiles {
        Ok(members) => members.len() > 1,
        Err(_) => true,
    }
}

/// Everything one presence observation needs from outside.
///
/// **The fields are private and [`for_poll`](PresenceInputs::for_poll) is the
/// only way in from another crate.** Every one of them is an input to a claim
/// about where a person is, and the freshness window in particular is not a
/// caller's to choose -- when it was, the publisher named it, and a publisher
/// naming it is a publisher that can get it wrong unobserved.
pub struct PresenceInputs<'a> {
    sessions: &'a [PresenceEvidence<'a>],
    /// Whether this pond has more than one household member. See
    /// [`household_has_multiple_members`] for what a failed read must answer.
    household_has_multiple_members: bool,
    /// Always [`PRESENCE_WINDOW`]. Kept as a field rather than read from the
    /// constant at the comparison so this module's own tests can place a
    /// fixture either side of a window they name.
    presence_window: Duration,
    now: DateTime<Utc>,
}

impl<'a> PresenceInputs<'a> {
    /// Build the inputs for one poll of the publisher.
    ///
    /// Takes what the polling loop actually holds and supplies the rest, so
    /// the loop has no window to name.
    pub fn for_poll(
        sessions: &'a [PresenceEvidence<'a>],
        household_has_multiple_members: bool,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            sessions,
            household_has_multiple_members,
            presence_window: PRESENCE_WINDOW,
            now,
        }
    }
}

/// The evidence behind one believed-present member.
#[derive(Debug, Clone, PartialEq)]
struct Believed {
    source: IdentificationSource,
    confidence: Option<f32>,
    session_id: String,
    last_activity: DateTime<Utc>,
}

impl Believed {
    /// Whether this evidence should replace `held` for the same member.
    ///
    /// Two conversations can name one person in the same poll. The stronger
    /// rung wins, and on a tie the more recent one -- the same ordering
    /// `SessionIdentity::supersedes` applies within a single session, so the
    /// two cannot disagree about which claim is better.
    fn beats(&self, held: &Believed) -> bool {
        match self.source.rank().cmp(&held.source.rank()) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => self.last_activity > held.last_activity,
        }
    }

    /// Report this belief as an edge in `transition`'s direction.
    fn transition(
        &self,
        profile_id: &str,
        transition: PresenceTransition,
        at: DateTime<Utc>,
    ) -> ProfilePresence {
        ProfilePresence {
            profile_id: profile_id.to_string(),
            transition,
            source: self.source,
            confidence: self.confidence,
            session_id: self.session_id.clone(),
            at,
        }
    }
}

/// Turns attributed conversations into [`ProfilePresence`] edges.
///
/// Owned by the publisher task, which polls. Every decision it makes is in
/// [`observe`](PresenceObserver::observe).
#[derive(Debug, Clone, Default)]
pub struct PresenceObserver {
    /// Who the pond believes is here, and on what. `None` means no baseline
    /// has been taken yet.
    ///
    /// **A restart is not everybody arriving.** The pond restarts on every
    /// deploy, and a member whose conversation is still fresh would otherwise
    /// be announced as walking in each time -- an edge nobody crossed. The
    /// first observation records the level and publishes nothing, exactly as
    /// [`ActivityObserver::seeded_from`] does for conversations. The cost is
    /// real and worth stating: a member who genuinely arrives during the first
    /// poll after boot is recorded rather than announced.
    believed: Option<BTreeMap<String, Believed>>,
}

impl PresenceObserver {
    /// Begin observing a pond whose current occupancy is unknown.
    pub fn awaiting_baseline() -> Self {
        Self::default()
    }

    /// Fold one poll in and return the transitions it produced.
    ///
    /// Arrivals first, then departures, each in profile-id order, so the
    /// output is deterministic for a consumer and for a test.
    ///
    /// Usually empty. Somebody continuing to be here is not an event; neither
    /// is a re-identification of somebody already present, nor the same member
    /// opening a second conversation, nor their evidence being upgraded from a
    /// face match to an explicit "this is Liz". All four are the same person,
    /// still here.
    pub fn observe(&mut self, inputs: PresenceInputs<'_>) -> Vec<ProfilePresence> {
        let present = present_members(&inputs);
        let Some(previous) = self.believed.replace(present.clone()) else {
            return Vec::new(); // baseline
        };

        let mut out = Vec::new();
        for (profile_id, evidence) in &present {
            if !previous.contains_key(profile_id) {
                out.push(evidence.transition(profile_id, PresenceTransition::Arrived, inputs.now));
            }
        }
        for (profile_id, evidence) in &previous {
            if !present.contains_key(profile_id) {
                out.push(evidence.transition(profile_id, PresenceTransition::Departed, inputs.now));
            }
        }
        out
    }

    /// Fold one poll in, where reading the store may have failed.
    ///
    /// `Err` publishes nothing **and leaves the belief exactly as it was**, and
    /// the second half is the one that matters. Treating an unreadable row as
    /// unattributed would publish a departure nobody performed; taking the
    /// failure as a fresh start would announce everybody arriving again when
    /// the read recovered. A failed read is "I do not know", not "the house is
    /// empty" -- the same answer [`ActivityObserver::seeded_from`] gives, and
    /// for the same reason.
    ///
    /// This lives here rather than as a `continue` in the polling loop because
    /// a `continue` in a timer loop is a decision no test can reach.
    pub fn observe_read<E>(&mut self, read: Result<PresenceInputs<'_>, E>) -> Vec<ProfilePresence> {
        match read {
            Ok(inputs) => self.observe(inputs),
            Err(_) => Vec::new(),
        }
    }
}

/// Who the evidence says is here, right now.
///
/// The three refusals, in order, are the whole safety surface of this phase:
/// a conversation the pond opened for itself is not a person; a conversation
/// nobody has spoken in recently is not evidence of anybody's whereabouts; and
/// a speaker the resolver cannot name is not a member, whether it called them
/// `Guest` or `Household`.
fn present_members(inputs: &PresenceInputs<'_>) -> BTreeMap<String, Believed> {
    let mut present: BTreeMap<String, Believed> = BTreeMap::new();

    for evidence in inputs.sessions {
        // A cron line at 3am creates a session row, and `PUT /sessions/{id}/user`
        // will bind any session id it is given -- including that one. P1's
        // filter is therefore not theoretical here: without it, attributing a
        // scheduled run to a member makes the pond believe they are home
        // whenever the schedule fires.
        if !evidence.origin.is_human() {
            continue;
        }
        if !is_fresh(evidence.last_activity, inputs.presence_window, inputs.now) {
            continue;
        }

        // PAI-1's resolver, not a second opinion. It owns the one-member
        // `Household` fallback, the refusal to trust a profile id with no
        // provenance, and the guest boundary -- and if any of those changes,
        // presence follows without anyone remembering it exists.
        let resolved = identity_resolution::resolve(&identity_resolution::ResolutionInputs {
            // A background poll holds no request token, so the strongest rung
            // cannot be supplied here even now that PAI-1 P9 has built it: the
            // token belongs to an HTTP request and this is a timer. The rung
            // still reaches presence -- through the session row, the moment a
            // handler binds one at `PairedDevice` strength -- and the `source`
            // this event carries is what says which rung it was.
            paired_device_profile: None,
            session: evidence.identity,
            household_has_multiple_members: inputs.household_has_multiple_members,
        });
        let ProfileScope::Owner(profile_id) = resolved.scope else {
            continue;
        };
        // "" is not a name. The type's promise is that a presence event names
        // a member, and a blank id would keep the promise textually while
        // addressing nobody -- the shape a defaulted field lands on.
        if profile_id.trim().is_empty() {
            continue;
        }

        let candidate = Believed {
            source: resolved.source,
            confidence: evidence.identity.confidence,
            session_id: evidence.session_id.to_string(),
            last_activity: evidence.last_activity,
        };
        match present.entry(profile_id) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                if candidate.beats(slot.get()) {
                    slot.insert(candidate);
                }
            }
        }
    }
    present
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid timestamp")
    }

    const THRESHOLD: Duration = Duration::from_secs(15 * 60);

    /// A session id the scheduler really mints: `sched-{task_id}-{unix_ts}`,
    /// straight out of `AgentScheduleExecutor::run_agent_prompt`.
    const A_CRON_FIRE: &str = "sched-morning-summary-1700000300";

    fn inputs(idle_secs: u64) -> ActivityInputs {
        ActivityInputs {
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(idle_secs),
            idle_threshold: THRESHOLD,
            now: t(0),
        }
    }

    fn phases(events: &[SessionLifecycle]) -> Vec<SessionPhase> {
        events.iter().map(|e| e.phase).collect()
    }

    fn session(id: &str, created_at: DateTime<Utc>, updated_at: DateTime<Utc>) -> Session {
        let mut session = Session::new(id.to_string());
        session.created_at = created_at;
        session.updated_at = updated_at;
        session
    }

    fn start(id: &str, created_at: DateTime<Utc>) -> SessionStart<'_> {
        SessionStart {
            id,
            created_at,
            origin: SessionOrigin::of(id),
        }
    }

    fn observer_over(sessions: Vec<Session>) -> ActivityObserver {
        ActivityObserver::seeded_from::<()>(Ok(sessions))
    }

    /// A clock reading for a pond nobody has touched in this process: the
    /// in-process activity clock still holds its boot value.
    fn untouched_clock(now: DateTime<Utc>) -> PollClock {
        let started_at = Instant::now();
        PollClock {
            started_at,
            started_at_utc: t(0),
            in_process_at: started_at,
            now,
            idle_threshold: THRESHOLD,
        }
    }

    // ── Origin ───────────────────────────────────────────────────────────

    /// The defect this classification exists for. A cron line running at 3am
    /// creates a session row, and a session row read as presence is the pond
    /// fabricating a person.
    #[test]
    fn the_scheduler_s_own_conversations_are_not_a_person() {
        assert_eq!(SessionOrigin::of(A_CRON_FIRE), SessionOrigin::Machine);
        assert!(!SessionOrigin::of(A_CRON_FIRE).is_human());

        // The ids the human paths produce: a dashboard UUID, the CLI's own
        // name, and the id shape the voice child is started with.
        for human in [
            "9f0c3f4e-6b1a-4a1e-9a6f-0c1d2e3f4a5b",
            "cli-chat",
            "voice-session",
            "unscheduled",
        ] {
            assert_eq!(
                SessionOrigin::of(human),
                SessionOrigin::Human,
                "{human} is a person's conversation and must be readable as presence"
            );
        }
    }

    // ── Projection ───────────────────────────────────────────────────────

    /// Both halves, from one call. The arrival list *and* the activity clock
    /// have to lose the pond's own conversations, because the gate the second
    /// one opens is what lets an `Idle` be published at all.
    #[test]
    fn a_scheduled_run_reaches_neither_the_arrival_list_nor_the_activity_clock() {
        let rows = vec![session(A_CRON_FIRE, t(300), t(300))];
        let activity = human_activity(&rows);
        assert!(
            activity.starts.is_empty(),
            "the pond announced its own cron fire as a conversation somebody started: {:?}",
            activity.starts
        );
        assert_eq!(
            activity.newest_activity, None,
            "a cron fire bumped the activity clock, so the pond believes the user is here"
        );

        // Vacuity control: the same two rows with a person's id are kept, so
        // the assertions above are about the origin and not about a projection
        // that drops everything.
        let human = vec![session("sess-human", t(300), t(300))];
        let activity = human_activity(&human);
        assert_eq!(activity.starts.len(), 1);
        assert_eq!(activity.newest_activity, Some(t(300)));
    }

    #[test]
    fn a_mixed_store_keeps_only_the_person_s_rows() {
        let rows = vec![
            session("sess-human", t(100), t(100)),
            session(A_CRON_FIRE, t(300), t(300)),
        ];
        let activity = human_activity(&rows);
        assert_eq!(
            activity.starts.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec!["sess-human"]
        );
        assert_eq!(
            activity.newest_activity,
            Some(t(100)),
            "the newest row is the pond's own, and taking it would report activity nobody \
             performed"
        );
    }

    // ── The never-at-startup gate ────────────────────────────────────────

    /// The second half of the same defect: the cron fire's INSERT also bumps
    /// `max(sessions.updated_at)`, which is one of the two things that decide
    /// whether the pond has seen a user at all in this process lifetime. With
    /// the gate open, fifteen minutes later the bus says the user went quiet
    /// -- a complete synthetic presence cycle on an empty house.
    #[test]
    fn a_scheduled_run_does_not_open_the_never_at_startup_gate() {
        let clock = untouched_clock(t(600));
        let machine = vec![session(A_CRON_FIRE, t(300), t(300))];
        assert!(
            !poll_inputs(&machine, clock).saw_activity_since_start,
            "a cron fire opened the never-at-startup gate: the pond has seen nobody, and the \
             next quiet period will be published as the user going away"
        );

        // Vacuity control: the identical row with a person's id does open it,
        // so the assertion above is about the origin and not about a gate that
        // is wired shut.
        let human = vec![session("sess-human", t(300), t(300))];
        assert!(
            poll_inputs(&human, clock).saw_activity_since_start,
            "a real conversation persisted by the voice child must still count as activity"
        );
    }

    /// In-process activity is the other half of the same gate and must be
    /// unaffected by any of this: an HTTP route bumping the shared clock is a
    /// person whatever the session store holds.
    #[test]
    fn an_http_route_still_opens_the_gate_with_no_sessions_at_all() {
        let started_at = Instant::now();
        let clock = PollClock {
            started_at,
            started_at_utc: t(0),
            in_process_at: started_at + Duration::from_millis(1),
            now: t(600),
            idle_threshold: THRESHOLD,
        };
        assert!(poll_inputs(&[], clock).saw_activity_since_start);
    }

    // ── Arrivals ─────────────────────────────────────────────────────────

    #[test]
    fn a_new_conversation_is_announced_with_its_id() {
        let mut obs = observer_over(vec![]);
        let clock = untouched_clock(t(10));
        let events = obs.poll(&[session("sess-new", t(5), t(5))], clock);
        assert_eq!(phases(&events), vec![SessionPhase::Started]);
        assert_eq!(events[0].session_id.as_deref(), Some("sess-new"));
    }

    /// End to end through the public entry point: the pond's own conversation
    /// produces nothing at all, and the same poll with a person's id produces
    /// the arrival. This is the event `notes_for_next` tells P4 to read as a
    /// household member walking in.
    #[test]
    fn a_cron_fire_is_never_announced_as_somebody_arriving() {
        let mut obs = observer_over(vec![]);
        let clock = untouched_clock(t(400));
        let events = obs.poll(&[session(A_CRON_FIRE, t(300), t(300))], clock);
        assert!(
            events.is_empty(),
            "a scheduled task published {events:?}; P4 reads a Started as a person arriving"
        );

        let mut control = observer_over(vec![]);
        assert_eq!(
            phases(&control.poll(&[session("sess-human", t(300), t(300))], clock)),
            vec![SessionPhase::Started],
            "the same poll with a person's id must still announce the arrival"
        );
    }

    /// The observer refuses a machine row even when it is handed one directly,
    /// so the filter in `human_activity` is not the only thing standing
    /// between a cron tick and a fabricated presence event.
    #[test]
    fn the_observer_itself_refuses_a_machine_session() {
        let mut obs = observer_over(vec![]);
        let announced = obs.observe(&[start(A_CRON_FIRE, t(300))], inputs(0));
        assert!(
            announced.is_empty(),
            "a machine-origin start reached the transition rules and was announced: {announced:?}"
        );

        // Vacuity control for the line above: the same call with a person's
        // row does announce, so this is the origin check and not an observer
        // that has stopped announcing anything.
        assert_eq!(
            phases(&obs.observe(&[start("sess-human", t(300))], inputs(0))),
            vec![SessionPhase::Started]
        );
    }

    /// Every install after the first is a restart with history. Announcing it
    /// would hand P4 a hundred "the user just started a conversation" events
    /// on boot.
    #[test]
    fn a_restart_does_not_announce_the_existing_history() {
        let existing = vec![
            session("old-1", t(-300), t(-300)),
            session("old-2", t(-200), t(-200)),
        ];
        let mut obs = observer_over(existing.clone());
        let announced = obs.poll(&existing, untouched_clock(t(0)));
        assert!(
            announced.is_empty(),
            "a restart announced {} conversation(s) that already existed as newly started: {:?}",
            announced.len(),
            announced
                .iter()
                .map(|e| e.session_id.as_deref().unwrap_or("-"))
                .collect::<Vec<_>>()
        );

        // Vacuity control: the same list *without* the seed is announced, so
        // the assertion above is about the seed and not about an observer that
        // never announces anything.
        let mut unseeded = observer_over(vec![]);
        assert_eq!(unseeded.poll(&existing, untouched_clock(t(0))).len(), 2);
    }

    /// The direction the seed exists to fail in. A read error is not the same
    /// claim as "this pond has no conversations", and confusing the two
    /// announces every session the pond has ever held as newly started.
    #[test]
    fn a_failed_read_takes_its_baseline_from_the_first_poll_that_works() {
        let existing = vec![
            session("old-1", t(-300), t(-300)),
            session("old-2", t(-200), t(-200)),
        ];
        let mut obs = ActivityObserver::seeded_from::<&str>(Err("db is busy"));
        assert!(
            obs.poll(&existing, untouched_clock(t(0))).is_empty(),
            "a failed startup read replayed the pond's history as arrivals"
        );

        // And the baseline really was taken: the conversation that begins
        // after it is still announced, so the failure path is quiet rather
        // than deaf.
        let mut later = existing.clone();
        later.push(session("sess-new", t(60), t(60)));
        assert_eq!(
            phases(&obs.poll(&later, untouched_clock(t(60)))),
            vec![SessionPhase::Started]
        );
    }

    /// Two conversations opened inside the same wall-clock second. SQLite's
    /// `datetime('now')` has no fractional part, so both rows carry the
    /// *identical* timestamp -- under a `created_at > watermark` comparison the
    /// second one is dropped, silently and forever.
    #[test]
    fn two_conversations_in_the_same_second_are_both_announced() {
        let mut obs = observer_over(vec![session("first", t(0), t(0))]);
        let same_second = vec![
            session("first", t(0), t(0)),
            session("second", t(0), t(0)),
            session("third", t(0), t(0)),
        ];
        let announced: Vec<String> = obs
            .poll(&same_second, untouched_clock(t(1)))
            .iter()
            .filter_map(|e| e.session_id.clone())
            .collect();
        assert_eq!(
            announced,
            vec!["second", "third"],
            "a conversation that began in the same second as the last one it knew about was \
             never announced"
        );
    }

    /// `sqlite_session_storage :: parse_dt` invents `Utc::now()` for any
    /// timestamp it cannot parse, so a corrupt `created_at` reads as *now* on
    /// every poll. Against a timestamp watermark that is an unbounded stream
    /// of "the user just arrived", once a minute, forever.
    #[test]
    fn a_conversation_whose_timestamp_keeps_moving_is_announced_once() {
        let mut obs = observer_over(vec![]);
        let mut announcements = 0;
        for poll in 0..5 {
            let drifting = vec![session("sess-unparseable", t(poll * 60), t(poll * 60))];
            announcements += obs.poll(&drifting, untouched_clock(t(poll * 60))).len();
        }
        assert_eq!(
            announcements, 1,
            "one conversation was announced {announcements} times because its timestamp moved"
        );
    }

    #[test]
    fn each_conversation_is_announced_once_and_oldest_first() {
        let mut obs = observer_over(vec![]);
        let batch = vec![
            session("second", t(20), t(20)),
            session("first", t(10), t(10)),
        ];
        let events = obs.poll(&batch, untouched_clock(t(30)));
        assert_eq!(
            events
                .iter()
                .map(|e| e.session_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["first", "second"],
            "announced in the order the conversations began"
        );
        assert!(
            obs.poll(&batch, untouched_clock(t(30))).is_empty(),
            "both conversations are already accounted for"
        );
    }

    // ── The activity clock ───────────────────────────────────────────────

    /// The failure this whole gate exists to prevent: a pond that booted and
    /// was never touched is not an idle user, it is an unused machine. It must
    /// stay silent however long it sits, and however many times it is polled.
    ///
    /// **The polling window has to cross the idle threshold, and this is the
    /// third version of this test.** The first stepped `idle_for` by a minute
    /// over ten polls, topping out well short of the threshold -- so it passed
    /// with the `saw_activity_since_start` gate DELETED. The second crossed the
    /// threshold but passed `&[]` for the sessions, a fixture production does
    /// not produce: a pond with a schedule has rows, and they were the defect.
    #[test]
    fn an_untouched_pond_publishes_nothing() {
        let mut obs = observer_over(vec![]);
        let mut crossed_the_threshold = false;
        for poll in 0..10 {
            let idle_for = Duration::from_secs(10 * 60 * poll);
            crossed_the_threshold |= idle_for >= THRESHOLD;
            // The rows a pond with one morning-summary schedule accumulates
            // while nobody is home: one per fire, none of them a person.
            let cron_fires: Vec<Session> = (0..=poll)
                .map(|n| {
                    let id = format!("sched-morning-summary-{n}");
                    session(&id, t(n as i64 * 600), t(n as i64 * 600))
                })
                .collect();
            // The gate is computed by production code over those rows; only
            // `idle_for` is stepped by hand, because a monotonic `Instant`
            // cannot be moved into the past on a machine that booted a minute
            // ago and the threshold has to be crossed for this to prove
            // anything.
            let gate = poll_inputs(&cron_fires, untouched_clock(t(poll as i64 * 600)))
                .saw_activity_since_start;
            let events = obs.observe(
                &human_activity(&cron_fires).starts,
                ActivityInputs {
                    saw_activity_since_start: gate,
                    idle_for,
                    ..inputs(0)
                },
            );
            assert!(
                events.is_empty(),
                "an untouched pond published {events:?} at poll {poll}, {}s idle",
                idle_for.as_secs()
            );
        }
        assert!(
            crossed_the_threshold,
            "this test proves nothing unless its polls reach {}s of idle",
            THRESHOLD.as_secs()
        );
    }

    /// Vacuity control for the test above: the same observer, given activity,
    /// does publish. Without this the silence assertion would also pass
    /// against an observer that can never say anything at all.
    ///
    /// The first poll is deliberately past the threshold. Ungated, it would
    /// record an `Idle` baseline from the boot clock and the next poll would
    /// announce a `Resumed` nobody performed.
    #[test]
    fn the_same_observer_does_publish_once_activity_is_real() {
        let mut obs = observer_over(vec![]);
        assert!(
            obs.observe(
                &[],
                ActivityInputs {
                    saw_activity_since_start: false,
                    idle_for: Duration::from_secs(30 * 60),
                    ..inputs(0)
                }
            )
            .is_empty(),
            "an untouched pond is not an idle user, however long it has sat"
        );
        assert!(
            obs.observe(&[], inputs(0)).is_empty(),
            "the first observation after real activity is a baseline, not a Resumed"
        );
        assert_eq!(
            phases(&obs.observe(&[], inputs(16 * 60))),
            vec![SessionPhase::Idle],
            "and once there is a baseline to depart from, going quiet is published"
        );
    }

    #[test]
    fn the_first_observation_is_a_baseline_not_an_event() {
        let mut obs = observer_over(vec![]);
        assert!(obs.observe(&[], inputs(0)).is_empty());

        let mut already_quiet = observer_over(vec![]);
        assert!(
            already_quiet.observe(&[], inputs(60 * 60)).is_empty(),
            "a first poll that is already past the threshold is still a baseline"
        );
    }

    #[test]
    fn going_quiet_publishes_idle_exactly_once() {
        let mut obs = observer_over(vec![]);
        obs.observe(&[], inputs(0));
        assert_eq!(
            phases(&obs.observe(&[], inputs(15 * 60))),
            vec![SessionPhase::Idle],
            "idle fires the moment the threshold is reached"
        );
        assert!(
            obs.observe(&[], inputs(60 * 60)).is_empty(),
            "staying quiet is not a new transition"
        );
    }

    #[test]
    fn coming_back_publishes_resumed() {
        let mut obs = observer_over(vec![]);
        obs.observe(&[], inputs(0));
        obs.observe(&[], inputs(20 * 60));
        assert_eq!(
            phases(&obs.observe(&[], inputs(5))),
            vec![SessionPhase::Resumed]
        );
        assert!(
            obs.observe(&[], inputs(5)).is_empty(),
            "staying active is not a new transition"
        );
    }

    #[test]
    fn idle_secs_reports_the_quiet_before_the_transition() {
        let mut obs = observer_over(vec![]);
        obs.observe(&[], inputs(0));
        let idle = obs.observe(&[], inputs(17 * 60)).remove(0);
        assert_eq!(idle.idle_secs, 17 * 60);
        assert_eq!(
            idle.session_id, None,
            "the activity clock is not per-session"
        );
    }

    /// One arrival, one event. Opening a conversation after a gap is a
    /// `Started`, not a `Started` plus a `Resumed` describing the same person
    /// walking back to the same machine.
    #[test]
    fn a_new_conversation_while_idle_says_started_not_resumed() {
        let mut obs = observer_over(vec![]);
        obs.observe(&[], inputs(0));
        assert_eq!(
            phases(&obs.observe(&[], inputs(30 * 60))),
            vec![SessionPhase::Idle]
        );

        let events = obs.observe(&[start("after-the-gap", t(60))], inputs(0));
        assert_eq!(phases(&events), vec![SessionPhase::Started]);
    }

    #[test]
    fn phase_labels_are_stable() {
        assert_eq!(SessionPhase::Started.as_str(), "started");
        assert_eq!(SessionPhase::Idle.as_str(), "idle");
        assert_eq!(SessionPhase::Resumed.as_str(), "resumed");
    }
}

#[cfg(test)]
mod presence_tests {
    use super::*;

    /// Taken from the constant rather than restated, so a fixture placed one
    /// second inside the window stays one second inside it.
    const WINDOW: Duration = PRESENCE_WINDOW;

    /// A session id the scheduler really mints, and one that
    /// `PUT /sessions/{id}/user` will happily bind to a household member.
    const A_CRON_FIRE: &str = "sched-morning-summary-1700000300";

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("valid timestamp")
    }

    /// A conversation begun and last spoken in at the same moment -- the shape
    /// a fixture takes when the two clocks are not what it is about.
    ///
    /// **Every fixture in this module was once this shape**, which is how the
    /// one line that picks the activity clock could be pointed at `created_at`
    /// with the whole suite green. Where the difference is the point, use
    /// [`long_running`].
    fn session(id: &str, updated_at: DateTime<Utc>) -> Session {
        long_running(id, updated_at, updated_at)
    }

    /// A conversation begun at `created_at` and last spoken in at
    /// `updated_at` -- the shape of every conversation that outlives its own
    /// first minute.
    fn long_running(id: &str, created_at: DateTime<Utc>, updated_at: DateTime<Utc>) -> Session {
        let mut session = Session::new(id.to_string());
        session.created_at = created_at;
        session.updated_at = updated_at;
        session.profile_id = None; // the observer reads the identity, not this
        session
    }

    /// A conversation the store says is bound to a member.
    ///
    /// `sessions.profile_id` is what `set_session_identity` writes and what
    /// `get_session_identity` reads back, so a `Some` here is the row shape
    /// that makes an identity read worth issuing.
    fn attributed(id: &str, updated_at: DateTime<Utc>) -> Session {
        let mut session = session(id, updated_at);
        session.profile_id = Some("jerry".to_string());
        session
    }

    fn identity(source: IdentificationSource, who: Option<&str>) -> SessionIdentity {
        SessionIdentity {
            profile_id: who.map(str::to_string),
            source,
            confidence: match source {
                IdentificationSource::Face => Some(0.71),
                _ => None,
            },
        }
    }

    fn poll(
        observer: &mut PresenceObserver,
        sessions: &[PresenceEvidence<'_>],
        household_has_multiple_members: bool,
        now: DateTime<Utc>,
    ) -> Vec<ProfilePresence> {
        observer.observe(PresenceInputs {
            sessions,
            household_has_multiple_members,
            presence_window: WINDOW,
            now,
        })
    }

    /// An observer that has already taken its baseline over `sessions`.
    fn seeded(
        sessions: &[PresenceEvidence<'_>],
        household_has_multiple_members: bool,
        now: DateTime<Utc>,
    ) -> PresenceObserver {
        let mut observer = PresenceObserver::awaiting_baseline();
        let published = poll(&mut observer, sessions, household_has_multiple_members, now);
        assert!(
            published.is_empty(),
            "the first observation is a baseline and must publish nothing, not {published:?}"
        );
        observer
    }

    fn named(events: &[ProfilePresence]) -> Vec<(&str, PresenceTransition)> {
        events
            .iter()
            .map(|e| (e.profile_id.as_str(), e.transition))
            .collect()
    }

    // ── Arrival ──────────────────────────────────────────────────────────

    #[test]
    fn a_member_who_starts_talking_arrives_once_and_the_event_names_them() {
        let mut observer = seeded(&[], true, t(0));

        let row = session("sess-jerry", t(60));
        let who = identity(IdentificationSource::Face, Some("jerry"));
        let evidence = [PresenceEvidence::of(&row, &who)];

        let events = poll(&mut observer, &evidence, true, t(70));
        assert_eq!(named(&events), vec![("jerry", PresenceTransition::Arrived)]);
        assert_eq!(events[0].source, IdentificationSource::Face);
        assert_eq!(
            events[0].confidence,
            Some(0.71),
            "a face match's confidence must survive -- it is how a proposer weighs the claim"
        );
        assert_eq!(events[0].session_id, "sess-jerry");
        assert_eq!(events[0].at, t(70));

        assert!(
            poll(&mut observer, &evidence, true, t(80)).is_empty(),
            "still being here is not a new arrival"
        );
    }

    // ── The column presence is keyed on ──────────────────────────────────

    /// The phase's headline safety property, and the one every other fixture
    /// in this module is blind to. `sessions.created_at` says when a
    /// conversation *began*; `sessions.updated_at` says when somebody last
    /// *spoke* in it. Presence is a claim about a person, so it reads the
    /// second: keying it on `created_at` would publish nothing for a member
    /// talking right now in a conversation older than the window, and then
    /// `Departed` for them on the poll after.
    ///
    /// No mirror fixture (`updated_at` older than `created_at`) is written,
    /// deliberately -- that is a row no production path can produce, and a
    /// test whose fixture production cannot produce tests a system that does
    /// not exist. The vacuity control below is the producible half.
    #[test]
    fn presence_is_keyed_on_when_somebody_last_spoke_not_on_when_the_conversation_began() {
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));

        // Opened three hours ago, spoken in ten seconds ago.
        let live = long_running("sess-long", t(0) - chrono::Duration::hours(3), t(0));
        let mut observer = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(
                &mut observer,
                &[PresenceEvidence::of(&live, &jerry)],
                true,
                t(10)
            )),
            vec![("jerry", PresenceTransition::Arrived)],
            "a conversation opened three hours ago and spoken in ten seconds ago is somebody in \
             the room; keying presence on created_at instead of updated_at loses every \
             conversation older than the window and reports the member as gone"
        );

        // Vacuity control, and the producible half of the discrimination: the
        // same long-running conversation, silent for those three hours,
        // publishes nothing. Without it the assertion above would also pass
        // against an observer for which any long-running row arrives.
        let quiet = long_running(
            "sess-long-quiet",
            t(0) - chrono::Duration::hours(3),
            t(0) - chrono::Duration::hours(3),
        );
        let mut control = seeded(&[], true, t(0));
        assert!(
            poll(
                &mut control,
                &[PresenceEvidence::of(&quiet, &jerry)],
                true,
                t(10)
            )
            .is_empty(),
            "a conversation nobody has spoken in for three hours was published as presence"
        );
    }

    // ── The two invariants that decide who may be named ──────────────────

    /// Invariant 4. The tempting wrong move in a one-member pond: the resolver
    /// answers `Household`, there is exactly one member, so "it must be them".
    /// It must not be them -- nothing identified anybody, and a proposal
    /// addressed on that basis is addressed to whoever happened to be talking.
    #[test]
    fn an_unidentified_speaker_in_a_one_member_pond_is_not_presence() {
        let mut observer = seeded(&[], false, t(0));

        let row = session("sess-anon", t(60));
        let nobody = SessionIdentity::unknown();
        let events = poll(
            &mut observer,
            &[PresenceEvidence::of(&row, &nobody)],
            false,
            t(70),
        );
        assert!(
            events.is_empty(),
            "an unidentified speaker was published as {events:?}; the resolver answered \
             Household, which is a scope and not a person"
        );

        // Vacuity control: the same pond, the same poll, an identified speaker.
        // Without this the assertion above would also pass against an observer
        // that can never publish in a one-member pond at all.
        let mut control = seeded(&[], false, t(0));
        let who = identity(IdentificationSource::Explicit, Some("jerry"));
        assert_eq!(
            named(&poll(
                &mut control,
                &[PresenceEvidence::of(&row, &who)],
                false,
                t(70)
            )),
            vec![("jerry", PresenceTransition::Arrived)]
        );
    }

    /// Invariant 5. An unidentified person in a shared pond is a Guest, and a
    /// Guest's presence is not the household's presence.
    #[test]
    fn a_guest_is_not_presence() {
        let mut observer = seeded(&[], true, t(0));
        let row = session("sess-visitor", t(60));
        let nobody = SessionIdentity::unknown();
        let events = poll(
            &mut observer,
            &[PresenceEvidence::of(&row, &nobody)],
            true,
            t(70),
        );
        assert!(
            events.is_empty(),
            "a guest session published {events:?}; invariant 5 says a Guest generates none"
        );
    }

    /// A profile id whose provenance is missing is not evidence, and presence
    /// inherits that refusal from the resolver rather than restating it.
    #[test]
    fn a_profile_id_with_no_source_is_not_presence() {
        let mut observer = seeded(&[], true, t(0));
        let row = session("sess-odd", t(60));
        let unsourced = identity(IdentificationSource::Unknown, Some("jerry"));
        assert!(
            poll(
                &mut observer,
                &[PresenceEvidence::of(&row, &unsourced)],
                true,
                t(70)
            )
            .is_empty(),
            "a profile id with no source was trusted as a person being home"
        );
    }

    /// "" keeps the type's promise textually and addresses nobody.
    #[test]
    fn a_blank_profile_id_names_nobody() {
        let mut observer = seeded(&[], true, t(0));
        let row = session("sess-blank", t(60));
        let blank = identity(IdentificationSource::Explicit, Some("   "));
        assert!(
            poll(
                &mut observer,
                &[PresenceEvidence::of(&row, &blank)],
                true,
                t(70)
            )
            .is_empty(),
            "a blank profile id was published as a member arriving"
        );
    }

    // ── What produces an identification that is not a person arriving ────

    /// P1's trap, in this phase's shape. The scheduler mints a session row at
    /// 3am, and `PUT /sessions/{id}/user` will bind any id it is handed -- so
    /// an attributed cron fire is a fixture production can produce, not a
    /// hypothetical. Without the origin filter the pond would believe a member
    /// walks in every time their morning summary runs.
    #[test]
    fn a_scheduled_run_attributed_to_a_member_is_not_that_member_being_home() {
        let mut observer = seeded(&[], true, t(0));
        let cron = session(A_CRON_FIRE, t(60));
        let who = identity(IdentificationSource::Explicit, Some("jerry"));
        let events = poll(
            &mut observer,
            &[PresenceEvidence::of(&cron, &who)],
            true,
            t(70),
        );
        assert!(
            events.is_empty(),
            "the pond's own conversation was published as {events:?}; a proposer reads an \
             Arrived as a household member walking in"
        );

        // Vacuity control: the identical row and identity under a person's
        // session id does arrive, so this is the origin filter and not an
        // observer that has stopped publishing.
        let mut control = seeded(&[], true, t(0));
        let human = session("sess-human", t(60));
        assert_eq!(
            named(&poll(
                &mut control,
                &[PresenceEvidence::of(&human, &who)],
                true,
                t(70)
            )),
            vec![("jerry", PresenceTransition::Arrived)]
        );
    }

    /// The other "identification that is not an arrival": a face recognised in
    /// a picture rather than in the room. Binding an identity does not touch
    /// `sessions.updated_at`, so a photograph attributed to a conversation
    /// nobody has spoken in for hours leaves the evidence stale and publishes
    /// nothing.
    #[test]
    fn a_face_bound_to_a_conversation_that_went_quiet_hours_ago_is_not_a_person_in_the_room() {
        let mut observer = seeded(&[], true, t(0));
        let stale = session("sess-yesterday", t(60) - chrono::Duration::hours(3));
        let liz = identity(IdentificationSource::Face, Some("liz"));
        assert!(
            poll(
                &mut observer,
                &[PresenceEvidence::of(&stale, &liz)],
                true,
                t(70)
            )
            .is_empty(),
            "a face bound to a three-hour-old conversation was published as somebody arriving"
        );

        // Vacuity control: the same identity on a conversation somebody is
        // actually speaking in does arrive.
        let mut control = seeded(&[], true, t(0));
        let live = session("sess-now", t(60));
        assert_eq!(
            named(&poll(
                &mut control,
                &[PresenceEvidence::of(&live, &liz)],
                true,
                t(70)
            )),
            vec![("liz", PresenceTransition::Arrived)]
        );
    }

    /// The window's edge, both sides. Reached is stale; one second short of it
    /// is not.
    #[test]
    fn the_freshness_window_is_closed_at_its_far_end() {
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let window = i64::try_from(WINDOW.as_secs()).unwrap();

        let just_inside = session("sess-inside", t(0));
        let mut a = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(
                &mut a,
                &[PresenceEvidence::of(&just_inside, &jerry)],
                true,
                t(window - 1)
            )),
            vec![("jerry", PresenceTransition::Arrived)]
        );

        let at_the_edge = session("sess-edge", t(0));
        let mut b = seeded(&[], true, t(0));
        assert!(
            poll(
                &mut b,
                &[PresenceEvidence::of(&at_the_edge, &jerry)],
                true,
                t(window)
            )
            .is_empty(),
            "evidence exactly as old as the window still counted as somebody being here; the \
             pond publishes Idle at the same instant, so the two would disagree"
        );
    }

    // ── Departure ────────────────────────────────────────────────────────

    /// Absence is decided, not observed, and it is decided once.
    #[test]
    fn evidence_going_stale_publishes_one_departure() {
        let row = session("sess-jerry", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let evidence = [PresenceEvidence::of(&row, &jerry)];

        let mut observer = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(&mut observer, &evidence, true, t(10))),
            vec![("jerry", PresenceTransition::Arrived)]
        );

        let gone = poll(&mut observer, &evidence, true, t(20 * 60));
        assert_eq!(named(&gone), vec![("jerry", PresenceTransition::Departed)]);
        assert_eq!(
            gone[0].session_id, "sess-jerry",
            "a departure names the conversation that went quiet"
        );
        assert_eq!(
            gone[0].source,
            IdentificationSource::Explicit,
            "and the evidence the belief had rested on"
        );

        assert!(
            poll(&mut observer, &evidence, true, t(60 * 60)).is_empty(),
            "staying away is not a second departure"
        );
    }

    /// Releasing a binding (`DELETE /sessions/{id}/user`) ends the belief. The
    /// pond has not seen anybody leave -- it has stopped being able to say who
    /// is there, which for a proposer is the same instruction: stop addressing
    /// them.
    #[test]
    fn releasing_a_binding_ends_the_belief() {
        let row = session("sess-jerry", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let mut observer = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(
                &mut observer,
                &[PresenceEvidence::of(&row, &jerry)],
                true,
                t(10)
            )),
            vec![("jerry", PresenceTransition::Arrived)]
        );

        let released = SessionIdentity::unknown();
        assert_eq!(
            named(&poll(
                &mut observer,
                &[PresenceEvidence::of(&row, &released)],
                true,
                t(20)
            )),
            vec![("jerry", PresenceTransition::Departed)],
            "the binding was released and the pond went on believing he was here"
        );
    }

    // ── Re-identification is not an arrival ──────────────────────────────

    #[test]
    fn re_identifying_somebody_already_here_publishes_nothing() {
        let first = session("sess-one", t(0));
        let face = identity(IdentificationSource::Face, Some("jerry"));
        let mut observer = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(
                &mut observer,
                &[PresenceEvidence::of(&first, &face)],
                true,
                t(10)
            )),
            vec![("jerry", PresenceTransition::Arrived)]
        );

        // The same session, re-identified more strongly.
        let explicit = identity(IdentificationSource::Explicit, Some("jerry"));
        assert!(
            poll(
                &mut observer,
                &[PresenceEvidence::of(&first, &explicit)],
                true,
                t(20)
            )
            .is_empty(),
            "an upgrade from a face match to an explicit binding is the same person, still here"
        );

        // A second conversation opened by the same person.
        let second = session("sess-two", t(30));
        assert!(
            poll(
                &mut observer,
                &[
                    PresenceEvidence::of(&first, &explicit),
                    PresenceEvidence::of(&second, &explicit),
                ],
                true,
                t(40)
            )
            .is_empty(),
            "opening a second conversation is not arriving twice"
        );
    }

    /// Two conversations naming one member in one poll: the stronger rung is
    /// the one the event reports, whichever order they arrive in.
    #[test]
    fn the_strongest_evidence_wins_when_two_conversations_name_one_member() {
        let weak_row = session("sess-face", t(30));
        let strong_row = session("sess-explicit", t(0));
        let weak = identity(IdentificationSource::Face, Some("jerry"));
        let strong = identity(IdentificationSource::Explicit, Some("jerry"));

        for order in [
            [
                PresenceEvidence::of(&weak_row, &weak),
                PresenceEvidence::of(&strong_row, &strong),
            ],
            [
                PresenceEvidence::of(&strong_row, &strong),
                PresenceEvidence::of(&weak_row, &weak),
            ],
        ] {
            let mut observer = seeded(&[], true, t(0));
            let events = poll(&mut observer, &order, true, t(40));
            assert_eq!(named(&events), vec![("jerry", PresenceTransition::Arrived)]);
            assert_eq!(
                events[0].source,
                IdentificationSource::Explicit,
                "the weaker rung won on ordering; the newer face row is the one that would"
            );
            assert_eq!(events[0].session_id, "sess-explicit");
        }
    }

    /// The other half of that comparison, and the half `beats`'s doc-comment
    /// states an ordering for: on an equal rung the more recent conversation
    /// wins. Inverting it used to change nothing any test could see, because
    /// the only other fixture with two same-rung rows asserts emptiness.
    ///
    /// The ordering is not arbitrary. `SessionIdentity::supersedes` is
    /// `self.source.rank() <= existing.source.rank()`, so an equal-rung write
    /// replaces what the row held -- newer wins there too. If presence broke
    /// the tie the other way, the event would name an older conversation than
    /// the session row itself considers current.
    #[test]
    fn on_an_equal_rung_the_conversation_spoken_in_most_recently_wins() {
        let older = session("sess-older", t(0));
        let newer = session("sess-newer", t(30));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));

        for order in [
            [
                PresenceEvidence::of(&older, &jerry),
                PresenceEvidence::of(&newer, &jerry),
            ],
            [
                PresenceEvidence::of(&newer, &jerry),
                PresenceEvidence::of(&older, &jerry),
            ],
        ] {
            let mut observer = seeded(&[], true, t(0));
            let events = poll(&mut observer, &order, true, t(40));
            assert_eq!(named(&events), vec![("jerry", PresenceTransition::Arrived)]);
            assert_eq!(
                events[0].session_id, "sess-newer",
                "two conversations at the same rung named one member and the stale one won; \
                 `SessionIdentity::supersedes` breaks that tie the other way, so the event and \
                 the session row would disagree about which claim is current"
            );
        }
    }

    // ── A restart is not everybody arriving ──────────────────────────────

    #[test]
    fn a_restart_does_not_announce_the_household_as_arriving() {
        let jerry_row = session("sess-jerry", t(0));
        let liz_row = session("sess-liz", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let liz = identity(IdentificationSource::Face, Some("liz"));
        let live = [
            PresenceEvidence::of(&jerry_row, &jerry),
            PresenceEvidence::of(&liz_row, &liz),
        ];

        let mut observer = PresenceObserver::awaiting_baseline();
        let first = poll(&mut observer, &live, true, t(10));
        assert!(
            first.is_empty(),
            "a restart announced {first:?}; the pond restarts on every deploy and nobody \
             crossed a threshold"
        );

        // The baseline really was taken, so the observer is quiet rather than
        // deaf: the departure that follows is still published.
        assert_eq!(
            named(&poll(&mut observer, &live, true, t(30 * 60))),
            vec![
                ("jerry", PresenceTransition::Departed),
                ("liz", PresenceTransition::Departed),
            ]
        );
    }

    /// Vacuity control for the baseline above: an observer that already has one
    /// does publish those same two arrivals. Without it, "a restart announces
    /// nothing" would also pass against an observer that announces nothing.
    #[test]
    fn the_same_rows_do_arrive_once_a_baseline_exists() {
        let jerry_row = session("sess-jerry", t(0));
        let liz_row = session("sess-liz", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let liz = identity(IdentificationSource::Face, Some("liz"));

        let mut observer = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(
                &mut observer,
                &[
                    PresenceEvidence::of(&jerry_row, &jerry),
                    PresenceEvidence::of(&liz_row, &liz),
                ],
                true,
                t(10)
            )),
            vec![
                ("jerry", PresenceTransition::Arrived),
                ("liz", PresenceTransition::Arrived),
            ],
            "each member is addressed by name, arrivals in profile-id order"
        );
    }

    /// Arrivals before departures, so a handover between two members reads in
    /// the order a consumer would want it: who is here now, then who is not.
    #[test]
    fn one_member_replacing_another_publishes_both_edges_arrival_first() {
        let jerry_row = session("sess-jerry", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let mut observer = seeded(&[], true, t(0));
        poll(
            &mut observer,
            &[PresenceEvidence::of(&jerry_row, &jerry)],
            true,
            t(10),
        );

        let liz_row = session("sess-liz", t(20 * 60));
        let liz = identity(IdentificationSource::Face, Some("liz"));
        assert_eq!(
            named(&poll(
                &mut observer,
                &[
                    PresenceEvidence::of(&jerry_row, &jerry),
                    PresenceEvidence::of(&liz_row, &liz),
                ],
                true,
                t(20 * 60 + 10)
            )),
            vec![
                ("liz", PresenceTransition::Arrived),
                ("jerry", PresenceTransition::Departed),
            ]
        );
    }

    // ── What the publisher used to decide for itself ─────────────────────

    /// The window the publisher can no longer choose. It was a field the
    /// polling loop filled in, and setting it to twenty-four hours there left
    /// the whole workspace green.
    ///
    /// Presence and [`SessionPhase::Idle`] have to age out on the same
    /// threshold, or the bus says a member is still here after it has already
    /// said the pond went quiet.
    #[test]
    fn the_freshness_window_is_the_one_that_decides_idle() {
        assert_eq!(
            PresenceInputs::for_poll(&[], true, t(0)).presence_window,
            Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS),
            "presence ages evidence out on a different clock from the one that publishes Idle, \
             so the two will disagree about whether somebody is still at the pond"
        );
    }

    /// [`attribution_candidates`] saves the publisher a point query per row on
    /// a table that grows forever. It must therefore not be able to change the
    /// answer: whatever it drops, the observer would have dropped anyway.
    #[test]
    fn skipping_a_read_never_changes_who_is_published() {
        let now = t(0);
        let window = i64::try_from(WINDOW.as_secs()).unwrap();

        let rows = vec![
            attributed("sess-live", t(-10)),
            attributed("sess-stale", t(-window - 10)),
            attributed(A_CRON_FIRE, t(-10)),
            session("sess-unbound", t(-10)),
        ];
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let liz = identity(IdentificationSource::Face, Some("liz"));
        let nobody = SessionIdentity::unknown();
        let identity_of = |id: &str| -> &SessionIdentity {
            match id {
                "sess-unbound" => &nobody,
                "sess-stale" => &liz,
                _ => &jerry,
            }
        };

        let candidates = attribution_candidates(&rows, now);
        assert_eq!(
            candidates
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["sess-live"],
            "the read-avoidance kept the wrong rows: the pond's own cron fire, a conversation \
             silent past the window, and a row with no attribution to read are each a query \
             whose answer the observer discards"
        );

        let every_row: Vec<PresenceEvidence<'_>> = rows
            .iter()
            .map(|session| PresenceEvidence::of(session, identity_of(&session.id)))
            .collect();
        let read_only_the_candidates: Vec<PresenceEvidence<'_>> = candidates
            .iter()
            .map(|session| PresenceEvidence::of(session, identity_of(&session.id)))
            .collect();

        let mut expensive = seeded(&[], true, t(-60));
        let mut cheap = seeded(&[], true, t(-60));
        let from_every_row = expensive.observe(PresenceInputs::for_poll(&every_row, true, now));
        let from_the_candidates = cheap.observe(PresenceInputs::for_poll(
            &read_only_the_candidates,
            true,
            now,
        ));

        assert_eq!(
            named(&from_every_row),
            vec![("jerry", PresenceTransition::Arrived)],
            "vacuity control: reading every row must still publish somebody, or the comparison \
             below is between two empty lists"
        );
        assert_eq!(
            from_every_row, from_the_candidates,
            "skipping the identity read changed who the pond believes is here; it is an \
             optimisation and not a gate, so the two must agree exactly"
        );
    }

    /// On failure, access narrows. The publisher used to decide this inside a
    /// `match` arm in a timer loop.
    #[test]
    fn a_household_count_that_cannot_be_read_answers_more_than_one() {
        let unreadable: Result<Vec<u8>, ()> = Err(());
        assert!(
            household_has_multiple_members(&unreadable),
            "a profile list that could not be read was answered with `one member`, which is the \
             value that turns an unidentified speaker into the whole household"
        );

        // Vacuity control: a successful read still answers honestly, so the
        // assertion above is about the failure and not about a function that
        // always says true.
        assert!(!household_has_multiple_members::<u8, ()>(&Ok(vec![])));
        assert!(!household_has_multiple_members::<u8, ()>(&Ok(vec![1])));
        assert!(household_has_multiple_members::<u8, ()>(&Ok(vec![1, 2])));
    }

    /// And what that direction buys presence today: nothing. Saying so is the
    /// point -- a guard claiming more than it holds is worse than none.
    ///
    /// `identity_resolution::resolve` consults the count only in the fallback
    /// that answers `Household` or `Guest`, and presence publishes for neither,
    /// so both values produce the same events over the same rows. This is a
    /// tripwire rather than a guard: the day presence grows a `Household` path
    /// it fails, and the failure direction above stops being merely correct and
    /// starts being load-bearing.
    #[test]
    fn the_household_count_cannot_change_a_published_presence_event() {
        let named_row = session("sess-jerry", t(0));
        let anon_row = session("sess-anon", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let nobody = SessionIdentity::unknown();
        let evidence = [
            PresenceEvidence::of(&named_row, &jerry),
            PresenceEvidence::of(&anon_row, &nobody),
        ];

        let mut shared = seeded(&[], true, t(0));
        let mut alone = seeded(&[], false, t(0));
        let with_guests = poll(&mut shared, &evidence, true, t(10));
        let one_member = poll(&mut alone, &evidence, false, t(10));

        assert_eq!(
            named(&with_guests),
            vec![("jerry", PresenceTransition::Arrived)],
            "vacuity control: an attributed row must still publish, or this compares two empty \
             lists and would pass against an observer that has stopped speaking"
        );
        assert_eq!(
            with_guests, one_member,
            "the household count now changes a presence event, which it could not before; \
             whatever depends on it needs `household_has_multiple_members`'s failure direction \
             to be load-bearing rather than merely correct"
        );
    }

    /// A store that cannot be read is not an empty house, and it is not a
    /// fresh start either. Both wrong answers publish an edge nobody crossed.
    #[test]
    fn a_failed_read_publishes_nothing_and_leaves_the_belief_standing() {
        let row = session("sess-jerry", t(0));
        let jerry = identity(IdentificationSource::Explicit, Some("jerry"));
        let evidence = [PresenceEvidence::of(&row, &jerry)];

        // Direction one: the outage must not empty the house.
        let mut still_here = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(&mut still_here, &evidence, true, t(10))),
            vec![("jerry", PresenceTransition::Arrived)]
        );
        assert!(
            still_here.observe_read::<()>(Err(())).is_empty(),
            "an unreadable store published a transition of its own"
        );
        assert!(
            poll(&mut still_here, &evidence, true, t(20)).is_empty(),
            "the failed read was folded in as an empty house, so a member who never left \
             departed and then arrived again when the store came back"
        );

        // Direction two: nor may it forget. A reset baseline swallows the
        // departure that follows, which is the edge P4 acts on.
        let mut departs = seeded(&[], true, t(0));
        assert_eq!(
            named(&poll(&mut departs, &evidence, true, t(10))),
            vec![("jerry", PresenceTransition::Arrived)]
        );
        assert!(departs.observe_read::<()>(Err(())).is_empty());
        assert_eq!(
            named(&poll(&mut departs, &evidence, true, t(20 * 60))),
            vec![("jerry", PresenceTransition::Departed)],
            "the failed read dropped the belief it was holding, so the departure that followed \
             was never published"
        );

        // Vacuity control: an `Ok` read is still just `observe`, so the two
        // assertions above are about the `Err` arm and not about a method that
        // never publishes.
        let mut ok = seeded(&[], true, t(0));
        assert_eq!(
            named(&ok.observe_read::<()>(Ok(PresenceInputs::for_poll(&evidence, true, t(10))))),
            vec![("jerry", PresenceTransition::Arrived)]
        );
    }

    #[test]
    fn transition_labels_are_stable() {
        assert_eq!(PresenceTransition::Arrived.as_str(), "arrived");
        assert_eq!(PresenceTransition::Departed.as_str(), "departed");
    }
}
