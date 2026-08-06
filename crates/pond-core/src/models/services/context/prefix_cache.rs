//! Prefix-cache state — PAI-4 P5's cache-age axis.
//!
//! Compaction until now asked one question of the conversation ("does it
//! fit?") and one question of the clock (P3's verbatim horizon). It never
//! asked what state the engine's KV prefix was in, even though the repository
//! had already measured what moving that prefix costs: a 78-character
//! system-prompt delta bought every session a full re-prefill on its second
//! turn, 3.7 s on the Orin.
//!
//! The rule this module exists to express, from 3.3 of the design:
//!
//! > Recompact aggressively when the cache is already lost; otherwise prefer
//! > edits that keep the prefix byte-identical.
//!
//! Both halves matter and only one of them is new here. The *warm* half was
//! already built, by PAI-4 P3: the trimmer's age-weighted degradation fires
//! only once the conversation is over budget, and its own comment gives
//! invariant 4 as the reason — over budget the prefix was moving anyway, so
//! the only question left is what gets sacrificed. What P3 could not do was
//! tell the difference between "the prefix is warm" and "the prefix is
//! already gone", so it used *over budget* as a proxy for both. This module
//! supplies the real signal, and it is used in exactly one direction: to
//! RELAX that guard when the cache is provably cold. A cold turn is paying a
//! full re-prefill whatever we do, so degrading aged material in the same
//! breath is genuinely free — and it buys headroom the next warm turns get to
//! keep.
//!
//! # What this deliberately does not do
//!
//! It does not tighten anything on the warm path. Stripping stale
//! `<system-context>` blocks and refreshing the rolling summary are also front
//! edits and they still run on every turn, warm or cold. Gating them needs the
//! measurement in section 7 — how many tokens a front edit has to save before
//! it beats the prefill it costs — and inventing a threshold here without it
//! would be a number nobody could defend.
//!
//! It also does not carry a `turns_served` THRESHOLD. Invariant 4 protects "a
//! warm prefix that has served many turns", which invites a rule of the form
//! "served fewer than N turns, perturb it freely". That is the widening
//! direction and it has no measurement behind it either. The only use of
//! `turns_served` here is its zero case, which needs no threshold and cannot
//! be wrong: a prefix that has served NO turns has nothing to protect.

use std::time::{Duration, Instant};

/// Why the engine's KV prefix is no longer usable.
///
/// The six reasons are not a taxonomy invented here — each one names a place
/// in the live adapter that already destroys the prefix, and until this phase
/// each did so silently. Recording which one fired is what turns an invisible
/// tax into an opportunity: 3.3 calls this out for `ToolSetChanged`
/// specifically, where two sessions alternating on one model diverge at the
/// tools block and re-prefill on every switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationReason {
    /// A new provider object was built and swapped into the engine.
    ProviderRebuilt,
    /// The chat model itself changed. Distinct from `ProviderRebuilt` because
    /// a provider can be rebuilt for the SAME model — re-stamping the thinking
    /// flag does exactly that — and the two cost the same prefill but mean
    /// different things when reading a trace.
    ModelSwapped,
    /// A fresh engine session was created for an existing conversation and its
    /// history replayed into it. There is no cache to inherit.
    SessionResumed,
    /// The turn carries an image. Multimodal turns forfeit KV retention
    /// outright — the reason `MAX_HISTORY_REPLAY_IMAGES` is 1.
    MultimodalTurn,
    /// The tool set changed, which moves the tools block inside the prefix.
    ToolSetChanged,
    /// The static system prefix itself changed.
    PromptChanged,
}

impl InvalidationReason {
    /// Stable, lowercase name for structured logs. Not `Display`: this is a
    /// field value in a trace, not prose shown to anybody.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderRebuilt => "provider_rebuilt",
            Self::ModelSwapped => "model_swapped",
            Self::SessionResumed => "session_resumed",
            Self::MultimodalTurn => "multimodal_turn",
            Self::ToolSetChanged => "tool_set_changed",
            Self::PromptChanged => "prompt_changed",
        }
    }
}

/// What the compaction path is allowed to assume about the KV prefix.
///
/// Two values, not three. "Unknown" is a real situation — every agent that is
/// not the live Goose adapter is in it — but it is not a third policy, and
/// giving it one would be a third branch nobody could test against hardware.
/// It resolves through [`PrefixCacheState::posture_of`], in one place, and it
/// resolves to `Warm`: assuming a cache we cannot see is warm costs at most
/// the behaviour this repository shipped before P5, whereas assuming it is
/// cold spends re-prefills on a machine that may be mid-conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePosture {
    /// The prefix has served at least one turn and nothing has invalidated it.
    /// Front edits cost a real re-prefill; make them only when forced.
    Warm,
    /// The prefix is gone, or has never served a turn. A re-prefill is already
    /// being paid this turn, so recompaction is free.
    Cold,
}

/// The engine's static-prefix cache as the compaction path sees it.
///
/// Deliberately NOT per-session: the live adapter keeps exactly one
/// `last_prefix_hash`, because the static prefix is the system prompt plus the
/// tools block and neither is per-session state there. Modelling this finer
/// than the thing it describes would invent precision that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixCacheState {
    /// When the current prefix was built. Telemetry and the raw material for
    /// section 7's measurement; no decision in this module reads it, and that
    /// is why there is no clock call anywhere in this file.
    pub built_at: Instant,
    /// Hash of the current static prefix — the adapter's `prefix_hash`.
    pub hash: u64,
    /// Turns served off this prefix without a rebuild. The value signal from
    /// 3.3; see the module docs for why only its zero case is acted on.
    pub turns_served: u32,
    /// Why the prefix is currently cold, when it is. `None` once a turn has
    /// been served, which is the only evidence that the prefix actually took.
    pub invalidated_by: Option<InvalidationReason>,
}

impl PrefixCacheState {
    /// A prefix that has been built but has served nothing yet — which is
    /// `Cold`, correctly: at construction the engine has no cache either.
    pub fn new(hash: u64, built_at: Instant) -> Self {
        Self {
            built_at,
            hash,
            turns_served: 0,
            invalidated_by: None,
        }
    }

    /// Record that something destroyed the prefix.
    ///
    /// `turns_served` resets because the count describes the CURRENT prefix,
    /// and there is about to be a different one. Keeping it would let a prefix
    /// inherit the standing of the one it replaced, which is the exact
    /// arithmetic invariant 4 turns on.
    pub fn invalidate(&mut self, reason: InvalidationReason) {
        self.invalidated_by = Some(reason);
        self.turns_served = 0;
    }

    /// Record that a new prefix was installed.
    ///
    /// Clearing `invalidated_by` here is safe and is NOT the same as calling
    /// the result warm: the new prefix has served nothing, so `turns_served`
    /// is zero and [`posture`](Self::posture) still reports `Cold`. What the
    /// reason described was the death of the prefix this one replaces, and
    /// carrying it forward would attribute it to a prefix it never applied to.
    /// The account of it survives where it is useful — the adapter logs the
    /// reason at the moment it fires.
    pub fn rebuilt(&mut self, hash: u64, now: Instant) {
        self.hash = hash;
        self.built_at = now;
        self.turns_served = 0;
        self.invalidated_by = None;
    }

    /// Record that a turn was served off this prefix unchanged.
    ///
    /// **The clear is deferred by one turn, and that is the load-bearing part
    /// of this type.** The invalidation points do not all fire at the same
    /// place in a turn: the live adapter creates and hydrates an engine session
    /// (`SessionResumed`) hundreds of lines *before* it compares the static
    /// prefix hash. If serving a turn cleared the reason immediately, a session
    /// resumed and served within one turn would clear its own `SessionResumed`
    /// and read `Warm` — at exactly the moment 3.2 calls the cache "long gone",
    /// which is the single most valuable cold moment the design identifies.
    /// Requiring that a turn has ALREADY been served defers the clear to the
    /// next turn, which is the first one that can honestly claim the cache
    /// survived something.
    ///
    /// The cost is that a reason is sticky for one extra turn, so the state
    /// reports `Cold` once more than strictly necessary — after a multimodal
    /// turn, most visibly. That is a bounded over-count of a condition whose
    /// only consequence is permission to degrade material already past the
    /// verbatim horizon, and that degradation is idempotent. The opposite
    /// error — one turn of false `Warm` — silently forfeits the free
    /// recompaction this phase exists to take.
    pub fn serve_turn(&mut self) {
        if self.turns_served > 0 {
            self.invalidated_by = None;
        }
        self.turns_served = self.turns_served.saturating_add(1);
    }

    /// How long the current prefix has stood. Takes `now` rather than reading
    /// a clock so this module stays as testable as P4's resume gate.
    pub fn age_since(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.built_at)
    }

    /// The rule, on a state we can see.
    pub fn posture(&self) -> CachePosture {
        if self.invalidated_by.is_some() || self.turns_served == 0 {
            CachePosture::Cold
        } else {
            CachePosture::Warm
        }
    }

    /// The rule, on a state we may not be able to see — the single place
    /// "unknown" becomes a policy. See [`CachePosture`] for why it is `Warm`.
    pub fn posture_of(state: Option<&Self>) -> CachePosture {
        match state {
            Some(s) => s.posture(),
            None => CachePosture::Warm,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> PrefixCacheState {
        PrefixCacheState::new(0xabc, Instant::now())
    }

    /// A prefix nobody has used is not warm, and this is the case that needs
    /// no threshold to be right.
    #[test]
    fn a_prefix_that_has_served_nothing_is_cold() {
        let state = fresh();
        assert_eq!(state.turns_served, 0);
        assert_eq!(state.invalidated_by, None);
        assert_eq!(state.posture(), CachePosture::Cold);
    }

    #[test]
    fn one_served_turn_is_enough_to_be_warm() {
        let mut state = fresh();
        state.serve_turn();
        assert_eq!(state.turns_served, 1);
        assert_eq!(state.posture(), CachePosture::Warm);
    }

    /// All six reasons, each from a warm prefix, each landing cold and each
    /// recording itself. Section 7's "PrefixCacheState transitions for all six
    /// invalidation reasons" is this test.
    #[test]
    fn every_invalidation_reason_takes_a_warm_prefix_cold_and_names_itself() {
        let all = [
            InvalidationReason::ProviderRebuilt,
            InvalidationReason::ModelSwapped,
            InvalidationReason::SessionResumed,
            InvalidationReason::MultimodalTurn,
            InvalidationReason::ToolSetChanged,
            InvalidationReason::PromptChanged,
        ];
        for reason in all {
            let mut state = fresh();
            state.serve_turn();
            state.serve_turn();
            assert_eq!(
                state.posture(),
                CachePosture::Warm,
                "{reason:?}: setup failed to warm the prefix"
            );

            state.invalidate(reason);

            assert_eq!(
                state.posture(),
                CachePosture::Cold,
                "{reason:?} left the prefix warm"
            );
            assert_eq!(state.invalidated_by, Some(reason));
            assert_eq!(
                state.turns_served, 0,
                "{reason:?} let the new prefix inherit the old one's standing"
            );
        }

        // Six distinct log names, so a trace can tell them apart.
        let mut names: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 6);
    }

    /// A rebuilt prefix is cold because it has served nothing — `turns_served`
    /// carries that, not the reason. The adapter rebuilds the prefix and trims
    /// the history within one turn, so getting this wrong would make the whole
    /// rule unreachable on precisely the turns it is for.
    #[test]
    fn rebuilding_does_not_make_a_prefix_warm() {
        let mut state = fresh();
        state.serve_turn();
        state.serve_turn();
        assert_eq!(state.posture(), CachePosture::Warm);

        state.invalidate(InvalidationReason::PromptChanged);
        state.rebuilt(0xdef, Instant::now());

        assert_eq!(state.hash, 0xdef);
        assert_eq!(state.turns_served, 0);
        assert_eq!(state.posture(), CachePosture::Cold);
    }

    /// The turn after a rebuild is warm — one turn of prefill bought a cache,
    /// and this phase must not decline to use it.
    #[test]
    fn the_turn_after_a_rebuild_is_warm() {
        let mut state = fresh();
        state.invalidate(InvalidationReason::PromptChanged);
        state.rebuilt(7, Instant::now());
        assert_eq!(state.posture(), CachePosture::Cold);

        state.serve_turn();
        assert_eq!(state.posture(), CachePosture::Warm);
    }

    /// The ordering bug this type's `serve_turn` exists to prevent, in the
    /// shape the live adapter produces it: `SessionResumed` is recorded while
    /// the engine session is being hydrated, and the static prefix comparison
    /// that follows in the SAME turn finds the hash unchanged. An eager clear
    /// reports `Warm` on the resume 3.2 calls the cache "long gone" for.
    #[test]
    fn a_resume_served_in_the_same_turn_stays_cold() {
        let mut state = fresh();
        // A conversation that has been running: the prefix is warm.
        state.serve_turn();
        state.serve_turn();
        assert_eq!(state.posture(), CachePosture::Warm);

        // The engine session is recreated and rehydrated...
        state.invalidate(InvalidationReason::SessionResumed);
        // ...and later in the very same turn the static prefix is found
        // unchanged, so the adapter serves the turn off it.
        state.serve_turn();

        assert_eq!(
            state.posture(),
            CachePosture::Cold,
            "the resume cleared itself within its own turn"
        );
        assert_eq!(
            state.invalidated_by,
            Some(InvalidationReason::SessionResumed)
        );

        // The NEXT turn is the first that can honestly claim the cache held.
        state.serve_turn();
        assert_eq!(state.posture(), CachePosture::Warm);
        assert_eq!(state.invalidated_by, None);
    }

    /// An agent with no prefix cache to report is not an invitation to
    /// recompact its conversation.
    #[test]
    fn an_unseen_cache_is_treated_as_warm() {
        assert_eq!(PrefixCacheState::posture_of(None), CachePosture::Warm);

        let mut warm = fresh();
        warm.serve_turn();
        assert_eq!(
            PrefixCacheState::posture_of(Some(&warm)),
            CachePosture::Warm
        );
        assert_eq!(
            PrefixCacheState::posture_of(Some(&fresh())),
            CachePosture::Cold
        );
    }

    #[test]
    fn age_is_measured_against_a_supplied_now_and_never_goes_backwards() {
        let built = Instant::now();
        let state = PrefixCacheState::new(1, built);
        assert_eq!(state.age_since(built), Duration::ZERO);
        assert_eq!(
            state.age_since(built + Duration::from_secs(90)),
            Duration::from_secs(90)
        );
        // A `now` from before the prefix was built reads as age zero rather
        // than panicking, the same clock-skew rule `idle_gap_since` uses.
        // `checked_sub` because `Instant - Duration` panics when the result is
        // not representable, which is a real possibility seconds after boot
        // and would make this test flaky on exactly one machine in a thousand.
        if let Some(earlier) = built.checked_sub(Duration::from_secs(5)) {
            assert_eq!(state.age_since(earlier), Duration::ZERO);
        }
    }

    #[test]
    fn many_served_turns_accumulate_and_saturate_rather_than_wrapping() {
        let mut state = fresh();
        for _ in 0..30 {
            state.serve_turn();
        }
        assert_eq!(state.turns_served, 30);
        assert_eq!(state.posture(), CachePosture::Warm);

        state.turns_served = u32::MAX;
        state.serve_turn();
        assert_eq!(state.turns_served, u32::MAX);
        assert_eq!(state.posture(), CachePosture::Warm);
    }
}
