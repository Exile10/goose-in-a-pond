//! Context Growth Monitor — tracks context window fill rate per session.
//!
//! Monitors how fast the context window fills up during a conversation,
//! emitting warnings before the "context cliff" where quality degrades.
//! Designed for small-context models (3K-8K tokens) on Jetson Orin Nano.
//!
//! Pure Rust, no external dependencies — lives in `pond-core/src/services/`.

use std::collections::HashMap;
use std::sync::Mutex;

/// Maximum number of growth rate samples to retain per session.
/// Used to compute a rolling average of tokens-per-turn.
const MAX_GROWTH_SAMPLES: usize = 10;

/// Utilization percentage above which a warning message is emitted.
const WARNING_THRESHOLD_PCT: f32 = 60.0;

/// Utilization percentage above which compaction should be triggered.
const COMPACT_THRESHOLD_PCT: f32 = 75.0;

/// If the estimated remaining turns falls below this, trigger compaction
/// regardless of utilization percentage.
const MIN_TURNS_REMAINING: u32 = 3;

/// Recorded turns that must pass between two compaction passes on one session.
///
/// PAI-4 P6. Before this phase `should_compact` only decided whether to emit an
/// SSE frame, so firing it on every turn past 75% cost nothing. The moment it
/// drives a summarisation it is a *rate*, and the utilisation limb is monotone:
/// a session that crosses 75% stays above it, so an ungated rule would spend a
/// model call between every pair of turns on the device least able to afford
/// one — the same failure PAI-4 P4 guarded against on the time axis with
/// `MIN_RESUME_IDLE_SECS`. Three is the smallest number that leaves the pass
/// visibly cheaper than the turns around it, and it is deliberately the same
/// number as [`MIN_TURNS_REMAINING`]: a session with fewer turns left than the
/// cooldown gets exactly one pass before the trimmer takes over, which is what
/// the trimmer is for.
const COMPACTION_COOLDOWN_TURNS: u32 = 3;

/// Per-session context tracking state.
#[derive(Debug, Clone)]
pub struct ContextState {
    /// Current estimated total tokens consumed in this session's context.
    pub estimated_tokens: u32,
    /// Number of conversation turns completed in this session.
    pub turns: u32,
    /// The context window limit (in tokens) for the active model.
    pub context_limit: u32,
    /// Rolling window of per-turn token growth (tokens added each turn).
    /// Capped at [`MAX_GROWTH_SAMPLES`] entries; oldest are evicted.
    pub growth_rates: Vec<u32>,
    /// `turns` as it stood when a compaction pass was last claimed for this
    /// session, or `None` if none ever was. Drives the cooldown in
    /// [`ContextMonitor::claim_compaction`].
    pub turns_at_last_compaction: Option<u32>,
}

/// Snapshot of a session's context health, returned by
/// [`ContextMonitor::check_context_health`].
#[derive(Debug, Clone)]
pub struct ContextHealth {
    /// Percentage of the context window currently consumed (0.0 - 100.0).
    pub utilization_pct: f32,
    /// Average tokens added per turn (rolling window).
    pub avg_growth_rate: u32,
    /// Estimated number of turns remaining before the context window is full,
    /// based on the rolling average growth rate. `u32::MAX` when growth is zero.
    pub estimated_turns_remaining: u32,
    /// Whether the caller should trigger context compaction.
    /// `true` when `utilization_pct > 75%` OR `estimated_turns_remaining < 3`.
    pub should_compact: bool,
    /// Human-readable warning message, present when `utilization_pct > 60%`.
    pub warning: Option<String>,
}

/// Thread-safe monitor that tracks context window growth across sessions.
///
/// Designed for zero-overhead when disabled — the caller checks the
/// `context_monitor_enabled` setting before invoking any methods.
pub struct ContextMonitor {
    session_contexts: Mutex<HashMap<String, ContextState>>,
}

impl ContextMonitor {
    /// Create a new, empty context monitor.
    pub fn new() -> Self {
        Self {
            session_contexts: Mutex::new(HashMap::new()),
        }
    }

    /// Record a completed conversation turn for the given session.
    ///
    /// - `session_id`: the session to update
    /// - `estimated_tokens`: total tokens now consumed in this session's context
    /// - `context_limit`: the model's context window size (tokens)
    ///
    /// Calculates the per-turn growth delta and stores it in the rolling window.
    pub fn record_turn(&self, session_id: &str, estimated_tokens: u32, context_limit: u32) {
        let mut sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");

        let state = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| ContextState {
                estimated_tokens: 0,
                turns: 0,
                context_limit,
                growth_rates: Vec::with_capacity(MAX_GROWTH_SAMPLES),
                turns_at_last_compaction: None,
            });

        // Calculate growth delta (tokens added this turn)
        let growth = estimated_tokens.saturating_sub(state.estimated_tokens);

        // Update state
        state.estimated_tokens = estimated_tokens;
        state.context_limit = context_limit;
        state.turns += 1;

        // Maintain rolling window of growth rates
        if state.growth_rates.len() >= MAX_GROWTH_SAMPLES {
            state.growth_rates.remove(0);
        }
        state.growth_rates.push(growth);
    }

    /// Check the context health for the given session.
    ///
    /// Returns a [`ContextHealth`] snapshot with utilization, growth metrics,
    /// and compaction/warning flags. Returns a zero-state health check if the
    /// session has no recorded turns.
    pub fn check_context_health(&self, session_id: &str) -> ContextHealth {
        let sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");

        match sessions.get(session_id) {
            Some(s) => health_of(s),
            None => ContextHealth {
                utilization_pct: 0.0,
                avg_growth_rate: 0,
                estimated_turns_remaining: u32::MAX,
                should_compact: false,
                warning: None,
            },
        }
    }

    /// Claim the right to run one compaction pass for this session, or decline.
    ///
    /// PAI-4 P6 — the *acting* half of `should_compact`, and the reason it is a
    /// method rather than a caller-side `if`. Three things have to be true at
    /// once and they have to be true under one lock:
    ///
    /// 1. the session is genuinely under pressure (`should_compact`, recomputed
    ///    here rather than passed in, so a stale snapshot cannot authorise a
    ///    pass);
    /// 2. [`COMPACTION_COOLDOWN_TURNS`] recorded turns have passed since the
    ///    last claim;
    /// 3. nobody else is claiming concurrently — two turns of one session can
    ///    finish at once, and both would otherwise queue a summarisation ahead
    ///    of the user's next turn on a serial on-device engine.
    ///
    /// The cooldown is stamped on the **claim**, not on completion. What is
    /// being rationed is the model call, and that is spent whether or not the
    /// pass finds anything to summarise.
    ///
    /// Returns `false` for a session with no recorded turns — a pass needs
    /// something to compact.
    pub fn claim_compaction(&self, session_id: &str) -> bool {
        let mut sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");

        let Some(state) = sessions.get_mut(session_id) else {
            return false;
        };

        if !health_of(state).should_compact {
            return false;
        }

        if let Some(last) = state.turns_at_last_compaction {
            if state.turns.saturating_sub(last) < COMPACTION_COOLDOWN_TURNS {
                return false;
            }
        }

        state.turns_at_last_compaction = Some(state.turns);
        true
    }

    /// Record that a compaction pass changed the shape of this session's
    /// history, without pretending its context window went back to zero.
    ///
    /// This is deliberately **not** [`reset_session`](Self::reset_session), and
    /// the difference is the one thing in this phase that could not be taken
    /// from the design bullet as written. `reset_session` drops the whole entry,
    /// which is right when the session is gone. After a rolling-summary refresh
    /// the session is very much still here and its window did not shrink: the
    /// next turn reports the same utilisation to `record_turn`, so a full reset
    /// would clear the cooldown stamp and let the pass fire again immediately —
    /// exactly the between-every-pair-of-turns model call the cooldown exists to
    /// prevent.
    ///
    /// What is genuinely stale is the growth window: those samples measured a
    /// differently-shaped history. Dropping them takes
    /// `estimated_turns_remaining` back to "unknown" until fresh samples
    /// accumulate, which quiets that limb of `should_compact` while leaving the
    /// utilisation limb — the honest one — untouched.
    pub fn note_compacted(&self, session_id: &str) {
        let mut sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");
        if let Some(state) = sessions.get_mut(session_id) {
            state.growth_rates.clear();
        }
    }

    /// Clear all tracking state for the given session.
    ///
    /// Call this when the session itself goes away. Until PAI-4 P6 this had no
    /// production caller at all, so every session ever seen stayed in the map
    /// for the life of the process, and a deleted session that came back under
    /// the same id inherited the growth history of the conversation it replaced.
    pub fn reset_session(&self, session_id: &str) {
        let mut sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");
        sessions.remove(session_id);
    }
}

/// Derive a health snapshot from one session's recorded state.
///
/// Shared by [`ContextMonitor::check_context_health`] and
/// [`ContextMonitor::claim_compaction`] so the predicate that reports pressure
/// and the predicate that acts on it cannot drift apart.
fn health_of(state: &ContextState) -> ContextHealth {
    let utilization_pct = if state.context_limit == 0 {
        0.0
    } else {
        (state.estimated_tokens as f32 / state.context_limit as f32) * 100.0
    };

    let avg_growth_rate = if state.growth_rates.is_empty() {
        0
    } else {
        let sum: u32 = state.growth_rates.iter().sum();
        sum / state.growth_rates.len() as u32
    };

    let estimated_turns_remaining = if avg_growth_rate == 0 {
        u32::MAX
    } else {
        let remaining_tokens = state.context_limit.saturating_sub(state.estimated_tokens);
        remaining_tokens / avg_growth_rate
    };

    let should_compact = utilization_pct > COMPACT_THRESHOLD_PCT
        || (avg_growth_rate > 0 && estimated_turns_remaining < MIN_TURNS_REMAINING);

    let warning = if utilization_pct > WARNING_THRESHOLD_PCT {
        Some(format!(
            "Context window {:.0}% full ({}/{} tokens). ~{} turns remaining.",
            utilization_pct,
            state.estimated_tokens,
            state.context_limit,
            if estimated_turns_remaining == u32::MAX {
                "unlimited".to_string()
            } else {
                estimated_turns_remaining.to_string()
            },
        ))
    } else {
        None
    };

    ContextHealth {
        utilization_pct,
        avg_growth_rate,
        estimated_turns_remaining,
        should_compact,
        warning,
    }
}

impl Default for ContextMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_session_has_zero_utilization() {
        let monitor = ContextMonitor::new();
        let health = monitor.check_context_health("session-1");
        assert_eq!(health.utilization_pct, 0.0);
        assert_eq!(health.avg_growth_rate, 0);
        assert_eq!(health.estimated_turns_remaining, u32::MAX);
        assert!(!health.should_compact);
        assert!(health.warning.is_none());
    }

    #[test]
    fn utilization_increases_after_turns() {
        let monitor = ContextMonitor::new();

        // Context limit: 4096 tokens
        monitor.record_turn("s1", 500, 4096);
        let h1 = monitor.check_context_health("s1");
        assert!(h1.utilization_pct > 12.0 && h1.utilization_pct < 13.0);
        assert_eq!(h1.avg_growth_rate, 500);

        monitor.record_turn("s1", 1000, 4096);
        let h2 = monitor.check_context_health("s1");
        assert!(h2.utilization_pct > 24.0 && h2.utilization_pct < 25.0);
        // Average of [500, 500] = 500
        assert_eq!(h2.avg_growth_rate, 500);

        monitor.record_turn("s1", 1800, 4096);
        let h3 = monitor.check_context_health("s1");
        assert!(h3.utilization_pct > 43.0 && h3.utilization_pct < 44.0);
    }

    #[test]
    fn warning_fires_above_sixty_percent() {
        let monitor = ContextMonitor::new();

        // Below 60%: no warning
        monitor.record_turn("s1", 2400, 4096);
        let h1 = monitor.check_context_health("s1");
        assert!(h1.utilization_pct < 60.0);
        assert!(h1.warning.is_none());

        // Above 60%: warning present
        monitor.record_turn("s1", 2600, 4096);
        let h2 = monitor.check_context_health("s1");
        assert!(h2.utilization_pct > 60.0);
        assert!(h2.warning.is_some());
        assert!(h2.warning.as_ref().unwrap().contains("Context window"));
    }

    #[test]
    fn should_compact_fires_above_seventy_five_percent() {
        let monitor = ContextMonitor::new();

        // Gradually fill the context to stay below 75% AND keep enough
        // remaining turns so the <3 turns heuristic doesn't fire early.
        // Use 8192 context with moderate growth (~400/turn).
        monitor.record_turn("s1", 400, 8192);
        monitor.record_turn("s1", 800, 8192);
        monitor.record_turn("s1", 1200, 8192);
        monitor.record_turn("s1", 1600, 8192);
        monitor.record_turn("s1", 2000, 8192);

        let h1 = monitor.check_context_health("s1");
        // 2000/8192 = ~24.4% — well below 75%
        // avg growth = 400, remaining = (8192-2000)/400 = 15 turns — well above 3
        assert!(h1.utilization_pct < 75.0, "util={}", h1.utilization_pct);
        assert!(
            !h1.should_compact,
            "should not compact at {}%",
            h1.utilization_pct
        );

        // Jump to above 75%
        monitor.record_turn("s1", 6200, 8192);
        let h2 = monitor.check_context_health("s1");
        // 6200/8192 = ~75.7%
        assert!(h2.utilization_pct > 75.0, "util={}", h2.utilization_pct);
        assert!(
            h2.should_compact,
            "should compact at {}%",
            h2.utilization_pct
        );
    }

    #[test]
    fn should_compact_fires_when_few_turns_remaining() {
        let monitor = ContextMonitor::new();

        // Small context window with high growth: 3072 tokens, 1000/turn
        // After turn 1: 1000/3072 = ~32%, but remaining = 2072/1000 = 2 turns
        monitor.record_turn("s1", 1000, 3072);
        let h1 = monitor.check_context_health("s1");
        assert!(h1.utilization_pct < 75.0, "Utilization should be below 75%");
        assert!(
            h1.estimated_turns_remaining < MIN_TURNS_REMAINING,
            "Should have fewer than {} turns remaining, got {}",
            MIN_TURNS_REMAINING,
            h1.estimated_turns_remaining
        );
        assert!(
            h1.should_compact,
            "Should compact when estimated_turns_remaining < {}",
            MIN_TURNS_REMAINING
        );
    }

    #[test]
    fn estimated_turns_remaining_calculates_correctly() {
        let monitor = ContextMonitor::new();

        // 4096 context, 400 tokens per turn
        monitor.record_turn("s1", 400, 4096);
        monitor.record_turn("s1", 800, 4096);
        monitor.record_turn("s1", 1200, 4096);

        let h = monitor.check_context_health("s1");
        // avg growth = (400 + 400 + 400) / 3 = 400
        assert_eq!(h.avg_growth_rate, 400);
        // remaining = (4096 - 1200) / 400 = 7
        assert_eq!(h.estimated_turns_remaining, 7);
    }

    #[test]
    fn reset_clears_session_state() {
        let monitor = ContextMonitor::new();

        monitor.record_turn("s1", 2000, 4096);
        let h1 = monitor.check_context_health("s1");
        assert!(h1.utilization_pct > 0.0);

        monitor.reset_session("s1");

        let h2 = monitor.check_context_health("s1");
        assert_eq!(h2.utilization_pct, 0.0);
        assert_eq!(h2.avg_growth_rate, 0);
        assert_eq!(h2.estimated_turns_remaining, u32::MAX);
        assert!(!h2.should_compact);
        assert!(h2.warning.is_none());
    }

    #[test]
    fn growth_rates_capped_at_max_samples() {
        let monitor = ContextMonitor::new();

        // Record 15 turns — growth_rates should only keep the last 10
        for i in 1..=15u32 {
            monitor.record_turn("s1", i * 100, 8192);
        }

        let sessions = monitor.session_contexts.lock().unwrap();
        let state = sessions.get("s1").unwrap();
        assert_eq!(state.growth_rates.len(), MAX_GROWTH_SAMPLES);
        assert_eq!(state.turns, 15);
    }

    #[test]
    fn multiple_sessions_tracked_independently() {
        let monitor = ContextMonitor::new();

        monitor.record_turn("s1", 1000, 4096);
        monitor.record_turn("s2", 500, 8192);

        let h1 = monitor.check_context_health("s1");
        let h2 = monitor.check_context_health("s2");

        assert!(h1.utilization_pct > h2.utilization_pct);
        assert_eq!(h1.avg_growth_rate, 1000);
        assert_eq!(h2.avg_growth_rate, 500);
    }

    #[test]
    fn zero_context_limit_returns_zero_utilization() {
        let monitor = ContextMonitor::new();
        monitor.record_turn("s1", 100, 0);
        let h = monitor.check_context_health("s1");
        assert_eq!(h.utilization_pct, 0.0);
    }

    // ── PAI-4 P6: acting on should_compact ──────────────────────────────

    /// Drive one session above the 75% utilisation threshold.
    fn saturate(monitor: &ContextMonitor, session: &str) {
        monitor.record_turn(session, 7000, 8192);
    }

    #[test]
    fn a_session_under_no_pressure_cannot_claim_a_compaction() {
        let monitor = ContextMonitor::new();
        monitor.record_turn("s1", 500, 8192);
        assert!(
            !monitor.claim_compaction("s1"),
            "claimed a compaction at {}% utilisation",
            monitor.check_context_health("s1").utilization_pct,
        );
    }

    #[test]
    fn an_unknown_session_cannot_claim_a_compaction() {
        let monitor = ContextMonitor::new();
        assert!(!monitor.claim_compaction("never-seen"));
    }

    /// The guard this phase turns on. `should_compact` is monotone once
    /// utilisation crosses the threshold, so without the cooldown every turn
    /// past 75% would queue its own summarisation.
    #[test]
    fn a_saturated_session_claims_once_and_then_waits_out_the_cooldown() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");

        assert!(monitor.claim_compaction("s1"), "first claim must succeed");

        for turn in 1..COMPACTION_COOLDOWN_TURNS {
            saturate(&monitor, "s1");
            assert!(
                !monitor.claim_compaction("s1"),
                "claimed again only {turn} turn(s) into a {COMPACTION_COOLDOWN_TURNS}-turn cooldown \
                 - a still-saturated session would summarise between every pair of turns",
            );
        }

        saturate(&monitor, "s1");
        assert!(
            monitor.claim_compaction("s1"),
            "cooldown never expired after {COMPACTION_COOLDOWN_TURNS} turns",
        );
    }

    #[test]
    fn repeated_claims_within_one_turn_yield_exactly_one_pass() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");

        let granted = (0..5).filter(|_| monitor.claim_compaction("s1")).count();
        assert_eq!(
            granted, 1,
            "{granted} concurrent claims were granted for one turn",
        );
    }

    #[test]
    fn the_cooldown_is_per_session() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");
        saturate(&monitor, "s2");

        assert!(monitor.claim_compaction("s1"));
        assert!(
            monitor.claim_compaction("s2"),
            "one session's cooldown blocked another's",
        );
    }

    /// `note_compacted` must NOT behave like `reset_session`: the window did not
    /// shrink, so utilisation stays honest and the cooldown stays stamped.
    #[test]
    fn note_compacted_clears_growth_history_but_not_the_cooldown() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");
        assert!(monitor.claim_compaction("s1"));

        monitor.note_compacted("s1");

        let h = monitor.check_context_health("s1");
        assert_eq!(h.avg_growth_rate, 0, "growth samples survived a compaction");
        assert_eq!(h.estimated_turns_remaining, u32::MAX);
        assert!(
            h.utilization_pct > COMPACT_THRESHOLD_PCT,
            "note_compacted pretended the context window emptied ({}%)",
            h.utilization_pct,
        );

        saturate(&monitor, "s1");
        assert!(
            !monitor.claim_compaction("s1"),
            "note_compacted cleared the cooldown, so the next turn re-fired",
        );
    }

    #[test]
    fn note_compacted_on_an_unknown_session_is_a_no_op() {
        let monitor = ContextMonitor::new();
        monitor.note_compacted("never-seen");
        assert!(!monitor.claim_compaction("never-seen"));
    }

    /// The "on clear" half of the phase: a deleted session must not hand its
    /// cooldown or its growth history to whatever reuses the id.
    #[test]
    fn reset_session_clears_the_cooldown_too() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");
        assert!(monitor.claim_compaction("s1"));

        monitor.reset_session("s1");

        saturate(&monitor, "s1");
        assert!(
            monitor.claim_compaction("s1"),
            "a cleared session inherited the cooldown of the conversation it replaced",
        );
    }
}
