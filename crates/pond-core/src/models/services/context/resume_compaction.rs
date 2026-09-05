//! Compact-on-resume, PAI-4's time axis as a pure `should_run` gate; see
//! docs/architecture/pai/04-smart-compaction.md section 3.2. [`ResumeGateInputs::reopened`]
//! is the startup guard: a resume is a user action, never an elapsed duration, or boot
//! would compact every stored session. Fallbacks resolve toward not running, because too
//! small a threshold recompacts mid-conversation.

use chrono::{DateTime, Utc};
use std::time::Duration;

/// Default gap after which reopening a session counts as a resume: 30 minutes,
/// chosen against its neighbours. It is 15x the default `summary_idle_secs`
/// (120s), so the idle rolling summary has run many times first, and 2x
/// `INACTIVITY_THRESHOLD_SECS` (15 min), where consolidation calls the house asleep.
pub const RESUME_IDLE_THRESHOLD_SECS: u32 = 30 * 60;

/// Floor applied to a stored `resume_compaction_idle_secs`, guarding the hole
/// `interval_floor_from_hours` guards in `consolidation_schedule`: a stored `0`
/// would mean "every reopen is a resume". Five minutes is longer than any pause
/// inside a live exchange and shorter than "coming back to it".
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

/// Decide whether reopening a session should trigger a compaction pass now: at
/// most one per reopen, only for a session with history quiet longer than
/// `idle_threshold`, and only when hybrid compaction owns pruning. `NoSummariser`
/// is checked last so "eligible but no worker" reads differently from "ineligible".
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

/// Convert a stored `resume_compaction_idle_secs` into a threshold duration,
/// applying [`MIN_RESUME_IDLE_SECS`] as a floor. A `0` in the store must not
/// mean "compact on every reopen".
pub fn idle_threshold_from_secs(secs: u32) -> Duration {
    Duration::from_secs(u64::from(secs.max(MIN_RESUME_IDLE_SECS)))
}

/// How long a session has been quiet, from its last activity timestamp.
///
/// A timestamp in the future (clock skew, restored backup, wrong RTC) yields
/// `Duration::ZERO`, read as active now, so a live session is never recompacted.
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
