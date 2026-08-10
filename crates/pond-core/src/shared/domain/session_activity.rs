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

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::user_data::domain::session::Session;
use crate::user_data::services::consolidation_schedule as sched;

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
