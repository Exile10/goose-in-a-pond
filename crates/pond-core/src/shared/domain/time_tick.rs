//! Wall-clock ticks on the reactive event spine (PAI-7 P1).
//!
//! A pond driven only by sensors and by the user has no way to notice that
//! *nothing has happened*. [`TimeTick`] is the heartbeat later phases hang
//! "it is six and the freezer never reported" on. P1 publishes it and nothing
//! consumes it: the value of the phase is that the event exists and is true.
//!
//! Pure domain, and deliberately clockless -- the publisher in `pond-server`
//! owns the timer, everything here is a function of the numbers it is handed,
//! so the cadence is unit-testable without sleeping.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which wall-clock boundary a [`TimeTick`] marks.
///
/// **One variant, deliberately.** Section 3.1 of
/// `docs/architecture/pai/07-proactive-intelligence.md` also names dawn/dusk
/// and quiet-hours boundaries. P1 ships neither, because neither can be
/// computed truthfully today:
///
/// - **Dawn/dusk needs a location.** The only coordinates in `Settings` are
///   `weather_latitude`/`weather_longitude`, which default to `0.0` and stay
///   there on every install that onboarded with a place *name* -- the weather
///   adapter geocodes the name on demand, over the network. A tick derived
///   from those defaults would announce sunrise in the Gulf of Guinea, and
///   resolving the real coordinates would put a network call inside a
///   background timer.
/// - **Quiet hours do not exist.** There is no `quiet_hours` field on
///   `Settings`, and no other representation of them anywhere in the tree. P6
///   introduces them together with the speech gating that gives them meaning.
///
/// Adding either later is one variant and one match arm. Publishing a boundary
/// nobody can compute is more expensive than that, because it is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeBoundary {
    /// The local wall clock passed the top of an hour.
    Hour,
}

impl TimeBoundary {
    /// Short, stable label for structured logs and the event log.
    pub fn as_str(self) -> &'static str {
        match self {
            TimeBoundary::Hour => "hour",
        }
    }
}

/// The pond crossed a wall-clock boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeTick {
    pub boundary: TimeBoundary,
    /// When the boundary was observed, in UTC.
    pub at: DateTime<Utc>,
    /// Local wall-clock hour, `0..=23`, read from the same clock the rules
    /// engine evaluates its time windows against (the host's zone). This is
    /// carried rather than derived so every consumer answers "what hour is it
    /// here" the same way; `Settings::timezone` is deliberately not consulted,
    /// because two clocks disagreeing about "after sunset" is worse than one
    /// clock that is merely the host's.
    pub local_hour: u8,
}

/// Seconds from `now` until the top of the next hour.
///
/// **The only public form, and it takes the clock reading rather than two
/// numbers off it.** The publisher's one production call site used to pass
/// `(minute, second)` as bare `u32`s, which swap silently: read the other way
/// round, 12:34:56 waits 206 seconds instead of 1504, and the hourly heartbeat
/// becomes one every few minutes. That call site sits inside a timer loop, so
/// nothing observed it — an unguarded argument order was the whole of the
/// defect, and removing the argument is a better fix than testing it.
pub fn secs_to_next_hour_from<T: chrono::Timelike>(now: &T) -> u64 {
    secs_to_next_hour(now.minute(), now.second())
}

/// Seconds from `minute`:`second` past the hour until the top of the next hour.
///
/// Pure, so the publisher's cadence is testable without a timer. **Never
/// returns zero**: the publisher sleeps this long between ticks, and a
/// zero-length sleep turns that loop into a spin that would publish an
/// unbounded burst of ticks. A leap second (`second == 60`) and any other
/// out-of-range input therefore round to one second rather than to none.
///
/// Private: see [`secs_to_next_hour_from`].
fn secs_to_next_hour(minute: u32, second: u32) -> u64 {
    const HOUR: u64 = 3600;
    HOUR.saturating_sub(u64::from(minute) * 60 + u64::from(second))
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_of_the_hour_waits_a_full_hour() {
        assert_eq!(secs_to_next_hour(0, 0), 3600);
    }

    #[test]
    fn half_past_waits_half_an_hour() {
        assert_eq!(secs_to_next_hour(30, 0), 1800);
        assert_eq!(secs_to_next_hour(30, 30), 1770);
    }

    #[test]
    fn one_second_before_the_hour_waits_one_second() {
        assert_eq!(secs_to_next_hour(59, 59), 1);
    }

    /// A leap second, or any clock that reports past the end of the hour, must
    /// not produce a zero-length sleep -- that is the difference between one
    /// tick an hour and a spinning publisher.
    #[test]
    fn a_leap_second_never_yields_a_zero_wait() {
        for (minute, second) in [(59, 60), (60, 60), (u32::MAX, u32::MAX)] {
            let wait = secs_to_next_hour(minute, second);
            assert_ne!(
                wait, 0,
                "{minute}:{second} is past the end of the hour and produced a zero-length wait; \
                 the publisher sleeps on this, so zero is not one tick early, it is a spin that \
                 publishes an unbounded burst of ticks"
            );
            assert_eq!(wait, 1, "an out-of-range clock waits the one-second floor");
        }
    }

    /// Every reachable wall-clock position lands inside `1..=3600`, and only
    /// the top of the hour waits a whole hour. Written as a sweep rather than
    /// as three literals because a regression here is off-by-one at one end of
    /// the range, which a handful of chosen points can miss.
    #[test]
    fn every_position_in_the_hour_waits_between_one_second_and_an_hour() {
        for minute in 0..60 {
            for second in 0..60 {
                let wait = secs_to_next_hour(minute, second);
                assert!(
                    (1..=3600).contains(&wait),
                    "wait for {minute}:{second} was {wait}, outside 1..=3600"
                );
                assert_eq!(
                    wait == 3600,
                    minute == 0 && second == 0,
                    "only the top of the hour waits a full hour; {minute}:{second} waited {wait}"
                );
            }
        }
    }

    /// The publisher's actual entry point, at a time whose two halves cannot
    /// be confused for one another. 1504 is 12:34:56 read correctly; 206 is
    /// the same clock read as `(second, minute)` — the swap that turns an
    /// hourly heartbeat into one every three and a half minutes, inside a
    /// timer loop where nobody would see it.
    #[test]
    fn the_wait_is_read_off_the_clock_the_right_way_round() {
        let at = chrono::NaiveTime::from_hms_opt(12, 34, 56).expect("valid time");
        let wait = secs_to_next_hour_from(&at);
        assert_ne!(
            wait, 206,
            "the clock was read as (second, minute): 12:34:56 waits 1504s, not 206s"
        );
        assert_eq!(wait, 1504);
    }

    /// Vacuity control for the assertion above: the two readings really are
    /// different numbers, so `assert_ne!(wait, 206)` is a claim about the
    /// order and not a comparison that could never fail.
    #[test]
    fn the_two_readings_of_that_clock_are_different_numbers() {
        assert_eq!(secs_to_next_hour(34, 56), 1504);
        assert_eq!(secs_to_next_hour(56, 34), 206);
    }

    #[test]
    fn boundary_label_is_stable() {
        assert_eq!(TimeBoundary::Hour.as_str(), "hour");
    }
}
