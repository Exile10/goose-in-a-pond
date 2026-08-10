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
//! Idle is defined by the same threshold background consolidation uses. One
//! definition of "the user has gone quiet" per pond, not two: the phase that
//! will consume these events is gated by that module's `should_run`, and a
//! second, disagreeing threshold would mean the bus says the user left while
//! the gate says they are still here.
//!
//! Pure domain: [`ActivityObserver::observe`] is a function of the inputs it is
//! handed plus its own recorded phase, so every transition is unit-testable
//! without a clock, a database, or a bus.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
/// [`observe`](ActivityObserver::observe).
#[derive(Debug, Clone)]
pub struct ActivityObserver {
    phase: Option<Observed>,
    /// Newest `created_at` already announced (or already present at startup).
    watermark: Option<DateTime<Utc>>,
}

impl ActivityObserver {
    /// Begin observing a pond whose newest existing conversation was created
    /// at `newest_session_created_at`.
    ///
    /// Seeding is what stops a restart announcing the pond's entire chat
    /// history as newly started. `None` is the claim "this pond has no
    /// conversations at all", so a caller whose read *failed* must pass
    /// `Some(now)` rather than `None`.
    pub fn starting_from(newest_session_created_at: Option<DateTime<Utc>>) -> Self {
        Self {
            phase: None,
            watermark: newest_session_created_at,
        }
    }

    /// Fold one observation in and return the transitions it produced, in the
    /// order they happened.
    ///
    /// Usually empty: most polls see the same phase as the last one. A poll
    /// can produce several `Started` events when several conversations began
    /// inside one polling interval, and at most one activity-clock transition.
    pub fn observe(
        &mut self,
        sessions: &[SessionStart<'_>],
        inputs: ActivityInputs,
    ) -> Vec<SessionLifecycle> {
        let mut out = Vec::new();

        // ── New conversations ────────────────────────────────────────────
        // A session row appearing is itself evidence of activity (only a turn
        // creates one), so this half is not gated on
        // `saw_activity_since_start` -- it is gated by the watermark, which
        // cannot advance without a genuinely new row.
        let mut fresh: Vec<&SessionStart<'_>> = sessions
            .iter()
            .filter(|s| self.watermark.is_none_or(|w| s.created_at > w))
            .collect();
        fresh.sort_by_key(|s| s.created_at);
        for session in fresh {
            self.watermark = Some(session.created_at);
            out.push(SessionLifecycle {
                phase: SessionPhase::Started,
                session_id: Some(session.id.to_string()),
                at: inputs.now,
                idle_secs: inputs.idle_for.as_secs(),
            });
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

    /// The failure this whole gate exists to prevent: a pond that booted and
    /// was never touched is not an idle user, it is an unused machine. It must
    /// stay silent however long it sits, and however many times it is polled.
    ///
    /// **The polling window has to cross the idle threshold, and this is the
    /// second version of this test.** The first stepped `idle_for` by a minute
    /// over ten polls, topping out well short of the threshold — so it passed
    /// with the `saw_activity_since_start` gate DELETED, and proved only that
    /// an active pond is quiet while it is active. A mutation run caught it.
    /// Ten polls of ten minutes cross the threshold five times over.
    #[test]
    fn an_untouched_pond_publishes_nothing() {
        let mut obs = ActivityObserver::starting_from(Some(t(-10)));
        let mut crossed_the_threshold = false;
        for poll in 0..10 {
            let idle_for = Duration::from_secs(10 * 60 * poll);
            crossed_the_threshold |= idle_for >= THRESHOLD;
            let events = obs.observe(
                &[],
                ActivityInputs {
                    saw_activity_since_start: false,
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
        let mut obs = ActivityObserver::starting_from(Some(t(-10)));
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
        let mut obs = ActivityObserver::starting_from(None);
        assert!(obs.observe(&[], inputs(0)).is_empty());

        let mut already_quiet = ActivityObserver::starting_from(None);
        assert!(
            already_quiet.observe(&[], inputs(60 * 60)).is_empty(),
            "a first poll that is already past the threshold is still a baseline"
        );
    }

    #[test]
    fn going_quiet_publishes_idle_exactly_once() {
        let mut obs = ActivityObserver::starting_from(None);
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
        let mut obs = ActivityObserver::starting_from(None);
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
        let mut obs = ActivityObserver::starting_from(None);
        obs.observe(&[], inputs(0));
        let idle = obs.observe(&[], inputs(17 * 60)).remove(0);
        assert_eq!(idle.idle_secs, 17 * 60);
        assert_eq!(
            idle.session_id, None,
            "the activity clock is not per-session"
        );
    }

    #[test]
    fn a_new_conversation_is_announced_with_its_id() {
        let mut obs = ActivityObserver::starting_from(Some(t(-100)));
        let events = obs.observe(
            &[SessionStart {
                id: "sess-new",
                created_at: t(5),
            }],
            inputs(0),
        );
        assert_eq!(phases(&events), vec![SessionPhase::Started]);
        assert_eq!(events[0].session_id.as_deref(), Some("sess-new"));
    }

    /// Every install after the first is a restart with history. Announcing it
    /// would hand P4 a hundred "the user just started a conversation" events
    /// on boot.
    #[test]
    fn a_restart_does_not_announce_the_existing_history() {
        let existing = [
            SessionStart {
                id: "old-1",
                created_at: t(-300),
            },
            SessionStart {
                id: "old-2",
                created_at: t(-200),
            },
        ];
        let mut obs = ActivityObserver::starting_from(Some(t(-200)));
        let announced = obs.observe(&existing, inputs(0));
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
        let mut unseeded = ActivityObserver::starting_from(None);
        assert_eq!(unseeded.observe(&existing, inputs(0)).len(), 2);
    }

    #[test]
    fn each_conversation_is_announced_once_and_oldest_first() {
        let mut obs = ActivityObserver::starting_from(Some(t(0)));
        let batch = [
            SessionStart {
                id: "second",
                created_at: t(20),
            },
            SessionStart {
                id: "first",
                created_at: t(10),
            },
        ];
        let events = obs.observe(&batch, inputs(0));
        assert_eq!(
            events
                .iter()
                .map(|e| e.session_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["first", "second"],
            "announced in the order the conversations began"
        );
        assert!(
            obs.observe(&batch, inputs(0)).is_empty(),
            "the watermark advanced past both"
        );
    }

    /// One arrival, one event. Opening a conversation after a gap is a
    /// `Started`, not a `Started` plus a `Resumed` describing the same person
    /// walking back to the same machine.
    #[test]
    fn a_new_conversation_while_idle_says_started_not_resumed() {
        let mut obs = ActivityObserver::starting_from(Some(t(0)));
        obs.observe(&[], inputs(0));
        assert_eq!(
            phases(&obs.observe(&[], inputs(30 * 60))),
            vec![SessionPhase::Idle]
        );

        let events = obs.observe(
            &[SessionStart {
                id: "after-the-gap",
                created_at: t(60),
            }],
            inputs(0),
        );
        assert_eq!(phases(&events), vec![SessionPhase::Started]);
    }

    #[test]
    fn phase_labels_are_stable() {
        assert_eq!(SessionPhase::Started.as_str(), "started");
        assert_eq!(SessionPhase::Idle.as_str(), "idle");
        assert_eq!(SessionPhase::Resumed.as_str(), "resumed");
    }
}
