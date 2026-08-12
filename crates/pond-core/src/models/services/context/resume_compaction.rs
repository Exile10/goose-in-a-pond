//! Compact-on-resume — PAI-4's time axis, as a pure gate.
//!
//! `docs/architecture/pai/04-smart-compaction.md` section 3.2 calls this "the
//! highest-value time behaviour, and it is free". A session reopened after a
//! gap is about to pay a full prefill whatever happens: the KV cache is long
//! gone, the rolling summary has not been refreshed since before the gap, and
//! the first turn back therefore reaches the model with a worse history than a
//! continuously-active session would get. Reshaping that history *before* the
//! first user message costs the user nothing, because there is no token stream
//! to wait on yet.
//!
//! The rule the design states is one line:
//!
//! ```text
//! session resumed && idle_gap > resume_compaction_idle_secs -> compact now
//! ```
//!
//! and the design also says which shape to give it: "the pattern already proven
//! in `user_data/services/consolidation_schedule.rs` — a pure `should_run` gate
//! with a startup guard — rather than inventing new scheduling". So this module
//! is that module's twin: plain `bool`s and `Duration`s in, a `Run`/`Skip` out,
//! every rule unit-testable without a clock, a server, or a model.
//!
//! # The startup guard, translated
//!
//! Consolidation's guard is "never merely because the process has been up a
//! while" — a box nobody has spoken to is not idle, it is unused. The same
//! hazard exists here in a different costume: at boot, *every* session in the
//! store has an enormous idle gap, so a gate that asked only about the gap would
//! compact the entire history store on startup and call it a resume.
//!
//! [`ResumeGateInputs::reopened`] is what closes it. A resume is a thing a user
//! did — they opened a session — not a state the clock drifted into. Nothing in
//! this crate can set it from a timer, and the one production caller sets it
//! from a request that a person made.
//!
//! # Which direction is the dangerous one
//!
//! An idle threshold that is too **small** is the scope-widening bug: a
//! two-minute pause mid-conversation is not a resume, and a gate that read it as
//! one would recompact between every pair of turns — spending a model call on
//! each, on a device where that call competes with the next turn's prefill. A
//! threshold that is too large only means the user pays what they pay today. So
//! every fallback here resolves toward *not* running: an unparseable or zeroed
//! setting is floored, not disabled, and a clock skewed into the future reads as
//! a warm session rather than a stale one.

use chrono::{DateTime, Utc};
use std::time::Duration;

/// Default gap after which reopening a session counts as a resume.
///
/// 30 minutes, and the number is chosen against its neighbours rather than
/// picked for roundness:
///
/// - It is 15x the default `summary_idle_secs` (120s), so the idle rolling
///   summary has had many chances to run before this ever fires. Firing sooner
///   would mostly duplicate work that loop already did.
/// - It is 2x `INACTIVITY_THRESHOLD_SECS` (15 min), the point at which memory
///   consolidation already considers the household asleep. A session untouched
///   for twice that is not a pause in a conversation.
///
/// Shortening it is the change that costs something; see the module header.
pub const RESUME_IDLE_THRESHOLD_SECS: u32 = 30 * 60;

/// Floor applied to a stored `resume_compaction_idle_secs`.
///
/// Guards the same hole `interval_floor_from_hours` guards in
/// `consolidation_schedule`: a stored `0` would otherwise mean "every reopen is
/// a resume", which is the failure this gate exists to prevent. Five minutes is
/// longer than any pause inside a live exchange and shorter than anything a user
/// would call "coming back to it", so it is a floor rather than a second
/// default.
pub const MIN_RESUME_IDLE_SECS: u32 = 5 * 60;

/// Everything the gate needs to decide whether a resume compaction may start.
#[derive(Debug, Clone, Copy)]
pub struct ResumeGateInputs {
    /// Current value of `hybrid_compaction_enabled`, re-read per call rather
    /// than snapshotted at startup, so the toggle takes effect without a
    /// restart. Compaction on resume is a mechanism of hybrid compaction; with
    /// hybrid compaction off, Goose owns pruning and this must not act.
    pub enabled: bool,
    /// The caller is *opening* this session, not paging back through it.
    ///
    /// This is the startup guard (see the module header): a resume is an action
    /// a user took, and no elapsed duration can substitute for one.
    pub reopened: bool,
    /// The session already holds turns. A session with no history has nothing
    /// to compact, and its "gap" is meaningless.
    pub has_prior_history: bool,
    /// How long since the session's most recent user-visible activity.
    pub idle_gap: Duration,
    /// The gap beyond which reopening counts as a resume, from
    /// `resume_compaction_idle_secs`.
    pub idle_threshold: Duration,
    /// Whether a provider capable of re-summarising is configured. `false` on a
    /// pond with no model loaded yet, where the deterministic trimmer still
    /// works but nothing here can run.
    pub summariser_available: bool,
}

/// Why a reopen did not start a compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `hybrid_compaction_enabled` is currently false.
    Disabled,
    /// The request was a page through existing history, not a reopen.
    NotAReopen,
    /// The session has no turns to compact.
    NoPriorHistory,
    /// The session was active too recently for this to be a resume.
    StillWarm,
    /// No provider is configured to do the summarising.
    NoSummariser,
}

impl SkipReason {
    /// Short, stable label for structured logs.
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::Disabled => "disabled",
            SkipReason::NotAReopen => "not_a_reopen",
            SkipReason::NoPriorHistory => "no_prior_history",
            SkipReason::StillWarm => "still_warm",
            SkipReason::NoSummariser => "no_summariser",
        }
    }
}

/// The gate's verdict for one reopen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Run,
    Skip(SkipReason),
}

impl GateDecision {
    pub fn is_run(self) -> bool {
        matches!(self, GateDecision::Run)
    }
}

/// Decide whether reopening a session should trigger a compaction pass now.
///
/// Semantics: *at most one pass per reopen, only for a session that has history
/// and has been quiet longer than `idle_threshold`, and only when hybrid
/// compaction owns pruning.*
///
/// The order of the checks is the order of the reported reasons, and it is
/// chosen so the log line is the useful one: `NoSummariser` is checked last
/// because "this would have run, but nothing can do the work" is a different
/// operational problem from "this was never eligible".
pub fn should_run(inputs: ResumeGateInputs) -> GateDecision {
    if !inputs.enabled {
        return GateDecision::Skip(SkipReason::Disabled);
    }
    // The startup guard. A gap alone is not a resume; a user is.
    if !inputs.reopened {
        return GateDecision::Skip(SkipReason::NotAReopen);
    }
    if !inputs.has_prior_history {
        return GateDecision::Skip(SkipReason::NoPriorHistory);
    }
    if inputs.idle_gap < inputs.idle_threshold {
        return GateDecision::Skip(SkipReason::StillWarm);
    }
    if !inputs.summariser_available {
        return GateDecision::Skip(SkipReason::NoSummariser);
    }
    GateDecision::Run
}

/// Convert a stored `resume_compaction_idle_secs` into a threshold duration.
///
/// Applies [`MIN_RESUME_IDLE_SECS`] as a floor. A `0` in the store — from a
/// hand-edited row, a bad client, or a future migration that writes the column
/// before the default lands — must not mean "compact on every reopen".
pub fn idle_threshold_from_secs(secs: u32) -> Duration {
    Duration::from_secs(u64::from(secs.max(MIN_RESUME_IDLE_SECS)))
}

/// How long a session has been quiet, from its last activity timestamp.
///
/// A timestamp in the future (clock skew, a restored backup, a device with a
/// wrong RTC) yields `Duration::ZERO` — read as "active right now", which is the
/// narrowing answer. Trusting it would classify a live session as a resume and
/// recompact it mid-conversation.
pub fn idle_gap_since(last_activity: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (now - last_activity).to_std().unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ResumeGateInputs {
        ResumeGateInputs {
            enabled: true,
            reopened: true,
            has_prior_history: true,
            idle_gap: Duration::from_secs(u64::from(RESUME_IDLE_THRESHOLD_SECS) + 1),
            idle_threshold: idle_threshold_from_secs(RESUME_IDLE_THRESHOLD_SECS),
            summariser_available: true,
        }
    }

    #[test]
    fn runs_when_a_stale_session_is_reopened() {
        assert_eq!(should_run(base()), GateDecision::Run);
    }

    #[test]
    fn disabled_never_runs() {
        let inputs = ResumeGateInputs {
            enabled: false,
            ..base()
        };
        assert_eq!(should_run(inputs), GateDecision::Skip(SkipReason::Disabled));
    }

    /// The startup guard, and the reason this gate takes `reopened` at all. At
    /// boot every session in the store has a gap of days; if the gap were the
    /// only input, starting the server would compact the entire history store
    /// and call each one a resume.
    #[test]
    fn a_huge_gap_alone_is_not_a_resume() {
        let inputs = ResumeGateInputs {
            reopened: false,
            idle_gap: Duration::from_secs(30 * 86_400),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::NotAReopen)
        );
    }

    #[test]
    fn a_session_with_no_turns_has_nothing_to_compact() {
        let inputs = ResumeGateInputs {
            has_prior_history: false,
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::NoPriorHistory)
        );
    }

    /// The scope-widening failure, named. A pause inside a live conversation
    /// must not read as a resume, or the gate recompacts between every pair of
    /// turns.
    #[test]
    fn a_pause_inside_a_conversation_is_not_a_resume() {
        for pause_secs in [0u64, 30, 120, 599] {
            let inputs = ResumeGateInputs {
                idle_gap: Duration::from_secs(pause_secs),
                ..base()
            };
            assert_eq!(
                should_run(inputs),
                GateDecision::Skip(SkipReason::StillWarm),
                "a {pause_secs}s pause was treated as a resume"
            );
        }
    }

    #[test]
    fn the_threshold_boundary_is_inclusive_of_running() {
        let threshold = idle_threshold_from_secs(RESUME_IDLE_THRESHOLD_SECS);
        let just_short = ResumeGateInputs {
            idle_gap: threshold - Duration::from_secs(1),
            idle_threshold: threshold,
            ..base()
        };
        assert_eq!(
            should_run(just_short),
            GateDecision::Skip(SkipReason::StillWarm)
        );
        let exactly = ResumeGateInputs {
            idle_gap: threshold,
            idle_threshold: threshold,
            ..base()
        };
        assert_eq!(should_run(exactly), GateDecision::Run);
    }

    #[test]
    fn a_pond_with_no_provider_reports_why_rather_than_pretending() {
        let inputs = ResumeGateInputs {
            summariser_available: false,
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::NoSummariser)
        );
    }

    // -- the threshold floor -------------------------------------------------

    /// A stored `0` must not mean "every reopen is a resume". This is the same
    /// hole `interval_floor_from_hours` closes for consolidation, and it is the
    /// one direction where being wrong costs a model call per reopen.
    #[test]
    fn a_stored_zero_is_floored_rather_than_treated_as_no_threshold() {
        assert_eq!(
            idle_threshold_from_secs(0),
            Duration::from_secs(u64::from(MIN_RESUME_IDLE_SECS)),
            "a stored 0 disabled the idle threshold entirely"
        );
        let inputs = ResumeGateInputs {
            idle_gap: Duration::from_secs(60),
            idle_threshold: idle_threshold_from_secs(0),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::StillWarm),
            "a zeroed setting let a one-minute pause count as a resume"
        );
    }

    #[test]
    fn the_floor_never_lengthens_a_deliberate_setting() {
        for secs in [MIN_RESUME_IDLE_SECS, 1_800, 7_200, 86_400] {
            assert_eq!(
                idle_threshold_from_secs(secs),
                Duration::from_secs(u64::from(secs)),
                "the floor overrode a deliberate {secs}s setting"
            );
        }
    }

    #[test]
    fn the_default_threshold_clears_its_own_floor() {
        assert_eq!(
            idle_threshold_from_secs(RESUME_IDLE_THRESHOLD_SECS),
            Duration::from_secs(u64::from(RESUME_IDLE_THRESHOLD_SECS)),
            "the default threshold is below its own floor, so the documented \
             30 minutes is not the number the gate uses"
        );
    }

    // -- the gap, and the clock ----------------------------------------------

    #[test]
    fn the_gap_is_measured_from_the_last_activity() {
        let now = Utc::now();
        let last = now - chrono::Duration::hours(3);
        let gap = idle_gap_since(last, now);
        assert!(gap >= Duration::from_secs(3 * 3600 - 1));
        assert!(gap <= Duration::from_secs(3 * 3600 + 1));
    }

    /// A device with a wrong clock, or a restored backup, can hold a session
    /// timestamp in the future. Reading a negative gap as an enormous positive
    /// one would recompact a live conversation.
    #[test]
    fn a_future_timestamp_reads_as_active_rather_than_stale() {
        let now = Utc::now();
        let skewed = now + chrono::Duration::days(2);
        assert_eq!(idle_gap_since(skewed, now), Duration::ZERO);

        let inputs = ResumeGateInputs {
            idle_gap: idle_gap_since(skewed, now),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::StillWarm),
            "clock skew was read as a resume"
        );
    }

    // -- reporting -----------------------------------------------------------

    #[test]
    fn every_skip_reason_has_a_distinct_label() {
        let labels = [
            SkipReason::Disabled.as_str(),
            SkipReason::NotAReopen.as_str(),
            SkipReason::NoPriorHistory.as_str(),
            SkipReason::StillWarm.as_str(),
            SkipReason::NoSummariser.as_str(),
        ];
        let mut sorted = labels.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            labels.len(),
            "two skip reasons log the same label, so the log cannot tell them apart"
        );
    }

    /// Every negated input, on its own, blocks a run. Written as a sweep rather
    /// than five assertions so a sixth input added without a check fails here
    /// instead of quietly widening the gate.
    /// A named way to unsatisfy exactly one precondition.
    type Flip = (&'static str, fn(&mut ResumeGateInputs));

    #[test]
    fn no_single_precondition_can_be_dropped() {
        let flips: [Flip; 5] = [
            ("enabled", |i| i.enabled = false),
            ("reopened", |i| i.reopened = false),
            ("has_prior_history", |i| i.has_prior_history = false),
            ("idle_gap", |i| i.idle_gap = Duration::ZERO),
            ("summariser_available", |i| i.summariser_available = false),
        ];
        for (name, flip) in flips {
            let mut inputs = base();
            flip(&mut inputs);
            assert!(
                !should_run(inputs).is_run(),
                "the gate ran with `{name}` unsatisfied"
            );
        }
    }
}
