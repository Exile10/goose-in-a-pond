//! Scheduling policy for background memory consolidation.
//!
//! Consolidation is expensive: on an 8GB Jetson it monopolises the single
//! on-device inference slot for one (single mode) or three (adversarial mode)
//! sequential LLM calls. It must therefore only ever run when the machine is
//! genuinely idle, and never merely because the process has been up a while.
//!
//! This module is deliberately pure — it takes plain `bool`s and `Duration`s
//! and returns a decision — so the two rules that were previously impossible to
//! test (the startup guard and the interval floor) can be unit-tested without
//! sleeping or spinning up a server.
//!
//! The three rules, in the order they are checked:
//!
//! 1. **Enabled.** Read from settings on *every* tick, never from a startup
//!    snapshot, so the Settings toggle takes effect without a restart.
//! 2. **Never on startup.** A run requires that real user activity has been
//!    observed *since this process started*. A freshly booted server that
//!    nobody has talked to is not "idle", it is unused — consolidating there
//!    burns power and risks mangling memories with no user present to notice.
//! 3. **Idle, and not too soon after the last run.** The user must have been
//!    quiet for `idle_threshold`, and at least `interval_floor` must have
//!    elapsed since the previous run.

use chrono::{DateTime, Utc};
use std::time::{Duration, Instant};

/// How long the user must be quiet before a consolidation may start.
///
/// Chosen to be comfortably longer than a natural pause in a conversation
/// (so consolidation does not steal the inference slot mid-exchange) while
/// still short enough that an evening of inactivity gets a pass in.
pub const INACTIVITY_THRESHOLD_SECS: u64 = 15 * 60;

/// Below this many scoreable memories there is nothing useful to consolidate:
/// merges need duplicates to find, and a tiny store is cheaper to leave alone.
pub const MIN_MEMORIES_TO_CONSOLIDATE: usize = 6;

/// Everything the gate needs to decide whether a run may start.
#[derive(Debug, Clone, Copy)]
pub struct GateInputs {
    /// Current value of `memory_consolidation_enabled`, re-read per tick.
    pub enabled: bool,
    /// Whether any real user activity has been observed since process start.
    /// This is the "never on startup" guard.
    pub saw_activity_since_start: bool,
    /// How long since the most recent observed user activity.
    pub idle_for: Duration,
    /// How long the user must be quiet before a run may start.
    pub idle_threshold: Duration,
    /// How long since the previous run in this process, or `None` if this
    /// process has not run consolidation yet.
    pub since_last_run: Option<Duration>,
    /// Minimum spacing between runs, from `memory_consolidation_interval_hours`.
    pub interval_floor: Duration,
}

/// Why a tick did not start a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `memory_consolidation_enabled` is currently false.
    Disabled,
    /// No user activity since this process started (the startup guard).
    NoActivitySinceStart,
    /// The user was active too recently.
    StillActive,
    /// A run happened less than `interval_floor` ago.
    IntervalFloor,
}

impl SkipReason {
    /// Short, stable label for structured logs.
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::Disabled => "disabled",
            SkipReason::NoActivitySinceStart => "no_activity_since_start",
            SkipReason::StillActive => "still_active",
            SkipReason::IntervalFloor => "interval_floor",
        }
    }
}

/// The gate's verdict for one scheduler tick.
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

/// Decide whether a background consolidation run may start now.
///
/// Semantics: *at most one run per `interval_floor`, and only after
/// `idle_threshold` of inactivity following actual user activity.*
pub fn should_run(inputs: GateInputs) -> GateDecision {
    if !inputs.enabled {
        return GateDecision::Skip(SkipReason::Disabled);
    }
    // "Never on startup" — an untouched process never consolidates, however
    // long it has been up.
    if !inputs.saw_activity_since_start {
        return GateDecision::Skip(SkipReason::NoActivitySinceStart);
    }
    if inputs.idle_for < inputs.idle_threshold {
        return GateDecision::Skip(SkipReason::StillActive);
    }
    if let Some(since) = inputs.since_last_run {
        if since < inputs.interval_floor {
            return GateDecision::Skip(SkipReason::IntervalFloor);
        }
    }
    GateDecision::Run
}

/// Has real user activity been observed since this process started?
///
/// This is the computation the "never on startup" guard rests on, and the exact
/// spot the old code got wrong: it initialised the activity clock to
/// `Instant::now()` at boot and then only ever asked "how long since that?",
/// which is indistinguishable from a genuine idle user.
///
/// Two sources, because the terminal voice loop runs in a **separate OS
/// process** and can never touch the server's in-memory clock:
///
/// - `in_process_at` — the shared `last_user_activity` clock, bumped by every
///   HTTP route. Compared **strictly** against `started_at` (captured after it,
///   during wiring) so the boot value can never masquerade as activity.
/// - `db_activity` — newest `sessions.updated_at`, which the voice child bumps
///   through `ChatService` on every turn it persists.
pub fn saw_activity_since_start(
    started_at: Instant,
    in_process_at: Instant,
    started_at_utc: DateTime<Utc>,
    db_activity: Option<DateTime<Utc>>,
) -> bool {
    in_process_at > started_at || db_activity.is_some_and(|at| at > started_at_utc)
}

/// How long the user has been quiet, taking the **most recent** of the two
/// activity sources.
///
/// `min` of the two idle durations, not `max`: if either source saw activity 10
/// seconds ago then the user has been idle for 10 seconds, whatever the other
/// source thinks. A `db_activity` timestamp in the future (clock skew) is
/// discarded rather than trusted.
pub fn combined_idle_for(
    in_process_at: Instant,
    db_activity: Option<DateTime<Utc>>,
    now_utc: DateTime<Utc>,
) -> Duration {
    let in_process_idle = in_process_at.elapsed();
    match db_activity.and_then(|at| (now_utc - at).to_std().ok()) {
        Some(db_idle) => in_process_idle.min(db_idle),
        None => in_process_idle,
    }
}

/// Convert `memory_consolidation_interval_hours` into a floor duration.
///
/// Guards against a stored `0`, which would otherwise disable rate limiting
/// entirely and let the scheduler re-fire on every tick.
pub fn interval_floor_from_hours(hours: u32) -> Duration {
    Duration::from_secs(u64::from(hours.max(1)) * 3600)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> GateInputs {
        GateInputs {
            enabled: true,
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(INACTIVITY_THRESHOLD_SECS + 1),
            idle_threshold: Duration::from_secs(INACTIVITY_THRESHOLD_SECS),
            since_last_run: None,
            interval_floor: interval_floor_from_hours(24),
        }
    }

    #[test]
    fn runs_when_idle_after_real_activity() {
        assert_eq!(should_run(base()), GateDecision::Run);
    }

    #[test]
    fn disabled_never_runs() {
        let inputs = GateInputs {
            enabled: false,
            ..base()
        };
        assert_eq!(should_run(inputs), GateDecision::Skip(SkipReason::Disabled));
    }

    /// E1: the whole point of the startup guard. A server that has been idle
    /// far longer than the threshold, but that nobody has interacted with since
    /// boot, must never consolidate.
    #[test]
    fn never_runs_on_startup_without_user_activity() {
        let inputs = GateInputs {
            saw_activity_since_start: false,
            idle_for: Duration::from_secs(86_400),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::NoActivitySinceStart)
        );
    }

    #[test]
    fn startup_guard_outranks_a_long_uptime() {
        // Even with no previous run and a week of uptime.
        let inputs = GateInputs {
            saw_activity_since_start: false,
            idle_for: Duration::from_secs(7 * 86_400),
            since_last_run: None,
            ..base()
        };
        assert!(!should_run(inputs).is_run());
    }

    #[test]
    fn does_not_run_while_user_is_active() {
        let inputs = GateInputs {
            idle_for: Duration::from_secs(60),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::StillActive)
        );
    }

    /// E1: honour `memory_consolidation_interval_hours` as a floor between
    /// runs. Previously nothing rate-limited repeats, so an idle box
    /// re-consolidated every ~15 minutes.
    #[test]
    fn interval_floor_blocks_a_repeat_run() {
        let inputs = GateInputs {
            since_last_run: Some(Duration::from_secs(3600)),
            interval_floor: interval_floor_from_hours(24),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::IntervalFloor)
        );
    }

    #[test]
    fn interval_floor_allows_a_run_once_elapsed() {
        let inputs = GateInputs {
            since_last_run: Some(Duration::from_secs(24 * 3600 + 1)),
            interval_floor: interval_floor_from_hours(24),
            ..base()
        };
        assert_eq!(should_run(inputs), GateDecision::Run);
    }

    #[test]
    fn interval_floor_treats_zero_hours_as_one_hour() {
        assert_eq!(
            interval_floor_from_hours(0),
            Duration::from_secs(3600),
            "a stored 0 must not disable rate limiting"
        );
        let inputs = GateInputs {
            since_last_run: Some(Duration::from_secs(59 * 60)),
            interval_floor: interval_floor_from_hours(0),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::IntervalFloor)
        );
    }

    #[test]
    fn first_run_is_not_blocked_by_the_floor() {
        let inputs = GateInputs {
            since_last_run: None,
            ..base()
        };
        assert_eq!(should_run(inputs), GateDecision::Run);
    }

    // ── saw_activity_since_start (the E1 bug site) ───────────────────────────

    /// Reproduces the original defect's setup: the activity clock is stamped at
    /// boot, *before* the scheduler captures its baseline, and nobody has
    /// interacted. This must read as "no activity".
    #[test]
    fn boot_stamped_clock_is_not_activity() {
        let in_process_at = Instant::now(); // stamped during wiring
        let started_at = Instant::now(); // scheduler baseline, captured after
        let started_at_utc = Utc::now();

        assert!(!saw_activity_since_start(
            started_at,
            in_process_at,
            started_at_utc,
            None
        ));
    }

    #[test]
    fn an_in_process_request_counts_as_activity() {
        let started_at = Instant::now();
        let started_at_utc = Utc::now();
        let in_process_at = started_at + Duration::from_millis(1);

        assert!(saw_activity_since_start(
            started_at,
            in_process_at,
            started_at_utc,
            None
        ));
    }

    /// The voice child is a separate process: its only visible trace is a
    /// session row newer than our start stamp.
    #[test]
    fn an_out_of_process_voice_turn_counts_as_activity() {
        let in_process_at = Instant::now();
        let started_at = in_process_at + Duration::from_millis(1);
        let started_at_utc = Utc::now();
        let voice_turn_at = started_at_utc + chrono::Duration::seconds(5);

        assert!(saw_activity_since_start(
            started_at,
            in_process_at,
            started_at_utc,
            Some(voice_turn_at)
        ));
    }

    #[test]
    fn pre_existing_sessions_do_not_count_as_activity() {
        let in_process_at = Instant::now();
        let started_at = in_process_at + Duration::from_millis(1);
        let started_at_utc = Utc::now();
        // History from a previous run of the server.
        let old_session = started_at_utc - chrono::Duration::days(3);

        assert!(
            !saw_activity_since_start(started_at, in_process_at, started_at_utc, Some(old_session)),
            "restarting with existing history must not licence a run"
        );
    }

    // ── combined_idle_for ───────────────────────────────────────────────────

    #[test]
    fn idle_takes_the_more_recent_source() {
        let in_process_at = Instant::now() - Duration::from_secs(3600);
        let now = Utc::now();
        // Out-of-process voice turn 10s ago while HTTP has been quiet an hour.
        let db = Some(now - chrono::Duration::seconds(10));

        let idle = combined_idle_for(in_process_at, db, now);
        assert!(
            idle < Duration::from_secs(60),
            "a recent voice turn must dominate a stale in-process clock, got {idle:?}"
        );
    }

    #[test]
    fn idle_falls_back_to_the_in_process_clock_without_db_activity() {
        let in_process_at = Instant::now() - Duration::from_secs(120);
        let idle = combined_idle_for(in_process_at, None, Utc::now());
        assert!(idle >= Duration::from_secs(120));
    }

    #[test]
    fn a_future_db_timestamp_is_discarded_rather_than_trusted() {
        let in_process_at = Instant::now() - Duration::from_secs(120);
        let now = Utc::now();
        let skewed = Some(now + chrono::Duration::hours(1));

        let idle = combined_idle_for(in_process_at, skewed, now);
        assert!(
            idle >= Duration::from_secs(120),
            "clock skew must not be read as activity"
        );
    }
}
