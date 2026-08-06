//! Which compaction mechanisms a model can afford — the gate PAI-4 dispatches on.
//!
//! [`CompactionProfile`](super::context_budget::CompactionProfile) already varies
//! the *budgets* by window size. It does not vary the *strategy*, so a 20 tok/s
//! model sharing a Jetson's GPU with the next turn's prefill and a hosted 128K
//! model are compacted by exactly the same code. This module is the missing
//! axis: given who is serving the model and how big its window resolved to, it
//! says which of the three compaction mechanisms are affordable.
//!
//! # Two axes, not one
//!
//! `docs/architecture/pai/04-smart-compaction.md` section 3.1 states the tiers as
//! three rows — "small on-device (`local`/`gguf`, <= 12K)", "medium (32K,
//! Ollama/llamafile)", "large / HTTP (>= 64K)". Read as written, each row mixes a
//! window size with a provider, and two real configurations fall through the
//! gaps: an 8K *hosted* model, and a 131K *Ollama* model on the Orin (which
//! `OllamaCatalogProvider` can now genuinely resolve, since PAI-3 P3 taught it to
//! read `context_length` from `/api/show`).
//!
//! So the table is implemented as the two independent questions it is really
//! made of:
//!
//! - **How big is the window?** [`SMALL_WINDOW_CEILING`] and
//!   [`LARGE_WINDOW_FLOOR`] bracket it. Both are anchors on the budget curve, not
//!   new numbers: 12,288 is `use_compact_prompt`'s boundary and `PROFILE_ANCHORS`'
//!   third point, and 65,536 is its fifth.
//! - **Whose GPU pays for a summarisation call?** [`runs_on_this_device`]. This is
//!   NOT the same predicate as `ContextGovernor`'s `is_local_provider`, and the
//!   difference is the point of this module. That one asks whether the *preamble*
//!   is re-prefilled locally every turn, which is true for the in-process engine
//!   and false for Ollama. This one asks whether an *extra* model call competes
//!   with the turn the user is waiting on, which is true for Ollama and llamafile
//!   too: on the Orin they are HTTP to `127.0.0.1` and the tokens come off the
//!   same 102 GB/s of memory bandwidth.
//!
//! Every row of the design's table is reproduced by the pair, and the two
//! undefined cells now have answers.
//!
//! # The one dangerous direction
//!
//! Only [`ModelClass::Large`] unlocks a mechanism (LLM re-summarisation), so
//! mis-classifying *upward* is the failure that costs something — a Jetson
//! stalling its own next prefill to re-summarise. Every fallback here therefore
//! resolves downward: an unrecognised provider is still barred from `Large` until
//! its window clears 64K, and no window at all classifies as `Small`.
//!
//! `WindowResolution::is_exact()` is deliberately *not* consulted. Exactness
//! decides whether budget arithmetic can be trusted to the token; it says nothing
//! about which mechanisms are affordable, and the one direction where being wrong
//! would hurt is already closed by the provider set.

use super::context_governor::WindowResolution;

/// Windows at or below this are the small tier.
///
/// 12,288 rather than a round 12,000 because it is already a boundary in this
/// module's neighbours: `CompactionProfile::use_compact_prompt` steps here, and
/// `PROFILE_ANCHORS` carries it as the top of the old tier-2 plateau. A tier
/// system with two nearly-equal boundaries would be a second copy of the same
/// decision, which is what PAI-3 exists to stop.
pub const SMALL_WINDOW_CEILING: usize = 12_288;

/// Windows at or below this stay out of the large tier however they are served.
///
/// The design says "large / HTTP (>= 64K)"; 64K in tokens is 65,536, which is
/// also `PROFILE_ANCHORS`' fifth point and the top of the old tier-3 plateau.
pub const LARGE_WINDOW_FLOOR: usize = 65_536;

/// Providers whose inference consumes THIS box's compute.
///
/// `ollama` and `llamafile` are in the set even though they speak HTTP, because
/// on a GIAP pond they speak it to `127.0.0.1`. A summarisation call to them is
/// not free work happening elsewhere; it is the same GPU the next turn needs.
///
/// The set is a deny-list for the large tier, so it is safe when it is too
/// *wide* and unsafe when it is too narrow. A provider that could point at
/// another machine (`OLLAMA_HOST`) is kept in it for that reason: assuming the
/// work is local is the narrowing assumption.
pub const ON_DEVICE_PROVIDERS: [&str; 4] = ["local", "gguf", "ollama", "llamafile"];

/// Whether a summarisation call to `provider` would compete with this pond's own
/// inference. See [`ON_DEVICE_PROVIDERS`].
pub fn runs_on_this_device(provider: &str) -> bool {
    ON_DEVICE_PROVIDERS
        .iter()
        .any(|p| provider.eq_ignore_ascii_case(p))
}

/// What a compaction path may do for a given [`ModelClass`].
///
/// Three flags rather than an opaque enum because the tiers differ by *which
/// mechanisms are added*, not by having unrelated implementations — and because
/// a caller asking "may I summarise here?" should not have to match on a tier
/// name to find out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionStrategy {
    /// The deterministic trimmer (`super::turn_trimmer`). On for every class and
    /// there is no configuration that turns it off: it is the only mechanism
    /// that cannot stall anything, which is invariant 1 stated as a default.
    pub deterministic_trim: bool,
    /// The idle rolling summary (`SessionSummaryService`). Runs after
    /// `summary_idle_secs`, never at startup, cancelled by a new turn; turns read
    /// its result and never wait for it.
    pub idle_rolling_summary: bool,
    /// LLM re-summarisation of the rolling summary itself, once it has grown
    /// stale. PAI-4 P2 builds it; nothing implements it yet.
    pub llm_resummarisation: bool,
}

/// How expensive an extra model call is for this model, on this box.
///
/// Ordered: a bigger variant is strictly more permissive, which is what makes
/// "a local provider can never reach the top" checkable as a comparison rather
/// than as a match arm somebody has to keep in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelClass {
    /// Window at or below [`SMALL_WINDOW_CEILING`], however it is served.
    ///
    /// The design names this row "small on-device", and on a pond it nearly
    /// always is. A small *hosted* window lands here too, and that is right for
    /// a different reason: at 12K there is no span worth summarising a summary
    /// of, so the mechanism the large tier unlocks would spend a model call to
    /// reclaim almost nothing.
    Small,
    /// Everything between the two boundaries, plus anything above
    /// [`LARGE_WINDOW_FLOOR`] that is served from this box.
    ///
    /// This is today's behaviour, and the second half is the cell the design's
    /// table did not have: a 131K Ollama model on the Orin gets the large tier's
    /// budgets from `CompactionProfile` and the medium tier's *strategy*, because
    /// the window is generous and the compute is not.
    Medium,
    /// Window at or above [`LARGE_WINDOW_FLOOR`], served from another box.
    ///
    /// The only class that permits an LLM in the compaction path. The call is
    /// cheap, it is somebody else's hardware, and it is off the critical path.
    Large,
}

impl ModelClass {
    /// A short label for logs, tests and the Models UI, matching
    /// `WindowSource::label`'s shape.
    pub fn label(&self) -> &'static str {
        match self {
            ModelClass::Small => "small",
            ModelClass::Medium => "medium",
            ModelClass::Large => "large",
        }
    }

    /// Classify from the provider and the window the governor resolved.
    ///
    /// Pure, and both arguments are things every budget call site already holds,
    /// so this needs no new plumbing to reach.
    pub fn classify(provider: &str, resolved_window_tokens: usize) -> Self {
        if resolved_window_tokens <= SMALL_WINDOW_CEILING {
            return ModelClass::Small;
        }
        if resolved_window_tokens >= LARGE_WINDOW_FLOOR && !runs_on_this_device(provider) {
            return ModelClass::Large;
        }
        ModelClass::Medium
    }

    /// Classify from a [`WindowResolution`] rather than a bare token count.
    ///
    /// The preferred entry point: it makes the governor the single source of the
    /// window here too, instead of letting a caller classify against a number it
    /// worked out some other way. That is the four-paths-disagree shape PAI-3
    /// removed, and it would come straight back if this only took a `usize`.
    pub fn from_resolution(provider: &str, resolution: &WindowResolution) -> Self {
        Self::classify(provider, resolution.tokens)
    }

    /// Which compaction mechanisms this class may use.
    ///
    /// # Why `idle_rolling_summary` is true on the small tier
    ///
    /// The design's table says the small tier is "deterministic trim only. No LLM
    /// in the compaction path", and invariant 3 repeats it. Its stated reason is
    /// that "a summarisation stall at 20 tok/s is a user-visible hang" — but the
    /// rolling summary is the one model call in this system that structurally
    /// cannot produce that hang. It runs only after `summary_idle_secs` of
    /// inactivity, it is cancelled by a new turn, and a turn reads whatever is in
    /// `sessions.rolling_summary` without ever awaiting it (section 1.1, and
    /// invariant 5 says the same thing).
    ///
    /// Setting it false here would therefore remove the rolling summary from the
    /// exact device that needs it most — the one whose window runs out first — to
    /// prevent a stall that path cannot cause. The phase brief is explicit that
    /// P1 preserves today's behaviour for the small and medium tiers, and today
    /// the idle summary runs regardless of tier, so it stays.
    ///
    /// There *is* a real on-device cost, and it is a different one: an idle
    /// summarisation warms the GPU and evicts the prefix cache, so the next turn
    /// after it pays a prefill it would not otherwise have paid. That is a
    /// PAI-4 P5 question — it is cache-age arithmetic, measurable, and it applies
    /// to the medium tier just as much. It is not the hang the design cites, and
    /// it should not be settled by leaving a flag flipped the wrong way here.
    pub fn strategy(&self) -> CompactionStrategy {
        CompactionStrategy {
            deterministic_trim: true,
            idle_rolling_summary: true,
            llm_resummarisation: matches!(self, ModelClass::Large),
        }
    }

    /// Whether any compaction mechanism for this class is allowed to call a
    /// model as part of *reshaping history* — that is, the re-summarisation the
    /// large tier unlocks, not the idle summary that produces the input to it.
    ///
    /// This is the question PAI-4 P2 asks, hoisted onto the class so P2's gate is
    /// one call rather than a match it has to keep aligned with this table.
    pub fn permits_compaction_model_call(&self) -> bool {
        self.strategy().llm_resummarisation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::services::context::context_governor::{
        ContextGovernor, ContextInputs, EngineWindow, WindowSource,
    };

    /// The design's table, cell by cell, plus the two cells it does not cover.
    /// This is the phase: everything else in this file is an argument about these
    /// rows.
    #[test]
    fn the_tier_table_classifies_every_row_the_design_states() {
        let cases: &[(&str, usize, ModelClass)] = &[
            // "Small on-device (local/gguf, <= 12K window)".
            ("local", 3_072, ModelClass::Small),
            ("local", 4_096, ModelClass::Small),
            ("gguf", 8_192, ModelClass::Small),
            ("local", 12_288, ModelClass::Small),
            // "Medium (32K, Ollama/llamafile)".
            ("ollama", 32_768, ModelClass::Medium),
            ("llamafile", 32_768, ModelClass::Medium),
            // The Orin's pinned window, which no row of the table covers.
            ("local", 16_384, ModelClass::Medium),
            // "Large / HTTP (>= 64K)".
            ("openai", 128_000, ModelClass::Large),
            ("anthropic", 200_000, ModelClass::Large),
            ("google", 65_536, ModelClass::Large),
            // Cell one the table leaves undefined: a SMALL hosted window. No span
            // worth re-summarising, so it takes the small strategy.
            ("openai", 8_192, ModelClass::Small),
            // Cell two: a LARGE window served from this box. PAI-3 P3 made this
            // reachable by teaching OllamaCatalogProvider to read /api/show.
            ("ollama", 131_072, ModelClass::Medium),
            ("llamafile", 131_072, ModelClass::Medium),
            ("local", 1_000_000, ModelClass::Medium),
        ];
        for &(provider, window, expected) in cases {
            assert_eq!(
                ModelClass::classify(provider, window),
                expected,
                "{provider} at {window} tokens classified as {}, expected {}",
                ModelClass::classify(provider, window).label(),
                expected.label()
            );
        }
    }

    /// The guard this phase exists to hold. Only the large tier may spend a model
    /// call on compaction; on either on-device tier that call competes with the
    /// prefill of the turn the user is waiting on.
    #[test]
    fn the_on_device_tiers_never_permit_an_llm_in_the_compaction_path() {
        for class in [ModelClass::Small, ModelClass::Medium] {
            let strategy = class.strategy();
            assert!(
                !strategy.llm_resummarisation,
                "the {} tier selected a strategy that re-summarises with an LLM; \
                 on this tier that call competes with the next turn's prefill \
                 (PAI-4 invariant 3)",
                class.label()
            );
            assert!(
                !class.permits_compaction_model_call(),
                "the {} tier reports that a compaction model call is permitted",
                class.label()
            );
        }
        assert!(
            ModelClass::Large.strategy().llm_resummarisation,
            "the large tier is the whole reason the dispatch exists and it \
             selected no re-summarisation"
        );
        assert!(ModelClass::Large.permits_compaction_model_call());
    }

    /// Invariant 1, as the one thing every tier has in common: the mechanism that
    /// cannot stall a turn is never the one that gets switched off.
    #[test]
    fn every_class_keeps_the_deterministic_trimmer() {
        for class in [ModelClass::Small, ModelClass::Medium, ModelClass::Large] {
            assert!(
                class.strategy().deterministic_trim,
                "{} lost the deterministic trimmer",
                class.label()
            );
        }
    }

    /// "Today's behaviour preserved for the small and medium tiers": today the
    /// idle rolling summary runs regardless of tier, and P1 changes nothing about
    /// that. See `strategy`'s doc comment for why the design's literal wording is
    /// not followed here.
    #[test]
    fn p1_changes_nothing_for_the_small_and_medium_tiers() {
        assert_eq!(
            ModelClass::Small.strategy(),
            ModelClass::Medium.strategy(),
            "P1 was meant to preserve today's behaviour on both on-device tiers, \
             and they now select different strategies"
        );
        assert!(ModelClass::Small.strategy().idle_rolling_summary);
        assert!(ModelClass::Medium.strategy().idle_rolling_summary);
        // The large tier adds a mechanism; it never removes one.
        let medium = ModelClass::Medium.strategy();
        let large = ModelClass::Large.strategy();
        assert!(large.deterministic_trim && large.idle_rolling_summary);
        assert_ne!(medium, large);
    }

    /// The safety property, as a range rather than a spot check. No window, and
    /// no future edit to the boundaries, may put an on-device provider in the one
    /// class that spends a model call.
    #[test]
    fn an_on_device_provider_can_never_reach_the_large_tier() {
        for provider in ON_DEVICE_PROVIDERS {
            for window in (0..=1_000_000usize).step_by(1_021) {
                let class = ModelClass::classify(provider, window);
                assert!(
                    class < ModelClass::Large,
                    "{provider} at {window} tokens reached the {} tier",
                    class.label()
                );
            }
        }
    }

    /// Case is not a classification decision. `chat_provider` is written by the
    /// settings UI, the onboarding lift and the CLI, and a capitalised spelling
    /// must not be the thing that unlocks an on-device summarisation call.
    #[test]
    fn provider_matching_ignores_case() {
        assert!(runs_on_this_device("Ollama"));
        assert!(runs_on_this_device("LLAMAFILE"));
        assert_eq!(
            ModelClass::classify("Ollama", 131_072),
            ModelClass::Medium,
            "a capitalised provider escaped the on-device deny-list"
        );
    }

    /// A provider nobody has heard of gets the narrowing answer at every window
    /// below the large floor, and is only trusted with the large tier once its
    /// window says it is a hosted model.
    #[test]
    fn an_unknown_provider_narrows_below_the_large_floor() {
        assert_eq!(ModelClass::classify("", 4_096), ModelClass::Small);
        assert_eq!(ModelClass::classify("", 0), ModelClass::Small);
        assert_eq!(
            ModelClass::classify("some-new-thing", 32_768),
            ModelClass::Medium
        );
        assert_eq!(
            ModelClass::classify("some-new-thing", 65_536),
            ModelClass::Large
        );
    }

    /// Both boundaries are inclusive on the side that keeps the model in the
    /// smaller tier, which is the narrowing reading of "<= 12K" and ">= 64K".
    #[test]
    fn the_boundaries_step_exactly_where_the_budget_curve_does() {
        assert_eq!(
            ModelClass::classify("openai", SMALL_WINDOW_CEILING),
            ModelClass::Small
        );
        assert_eq!(
            ModelClass::classify("openai", SMALL_WINDOW_CEILING + 1),
            ModelClass::Medium
        );
        assert_eq!(
            ModelClass::classify("openai", LARGE_WINDOW_FLOOR - 1),
            ModelClass::Medium
        );
        assert_eq!(
            ModelClass::classify("openai", LARGE_WINDOW_FLOOR),
            ModelClass::Large
        );
    }

    /// A bigger window must never buy a *less* capable strategy, or raising
    /// `context_window_override` could cost a hosted model its re-summarisation.
    #[test]
    fn the_class_is_non_decreasing_in_the_window() {
        for provider in ["local", "ollama", "openai", "unknown"] {
            let mut prev = ModelClass::classify(provider, 0);
            for window in (0..=300_000usize).step_by(311) {
                let class = ModelClass::classify(provider, window);
                assert!(
                    class >= prev,
                    "{provider}: the window grew to {window} and the class fell \
                     from {} to {}",
                    prev.label(),
                    class.label()
                );
                prev = class;
            }
        }
    }

    /// The classes are not derived from the budget curve, so this pins that they
    /// still agree with it where they overlap: the small tier is exactly the set
    /// of windows that also get the compact prompt.
    #[test]
    fn the_small_tier_is_exactly_the_compact_prompt_tier() {
        use crate::models::services::context::context_budget::CompactionProfile;
        for window in [0usize, 3_072, 4_096, 8_192, 12_288, 12_289, 32_768, 128_000] {
            let profile = CompactionProfile::from_context_window(window);
            assert_eq!(
                ModelClass::classify("local", window) == ModelClass::Small,
                profile.use_compact_prompt(),
                "window {window}: the small tier and the compact-prompt tier \
                 disagree, so there are two boundaries where there should be one"
            );
        }
    }

    // -- composed with the real governor, not with a hand-written number -------

    /// The classification has to survive being fed by the thing that will
    /// actually feed it. A fixture that passes a `usize` straight in proves the
    /// arithmetic and nothing about the composition.
    #[test]
    fn a_registry_pinned_orin_resolves_and_classifies_as_medium() {
        let inputs = ContextInputs {
            provider: "local",
            model: "gemma-4-e2b",
            registry_pinned: Some(16_384),
            ..Default::default()
        };
        let resolution = ContextGovernor::resolve(&inputs);
        assert_eq!(resolution.source, WindowSource::Registry);
        assert_eq!(
            ModelClass::from_resolution(inputs.provider, &resolution),
            ModelClass::Medium
        );
    }

    /// The engine's own report is the strongest rung, and a 3K Jetson that
    /// reports 3,072 must land in the small tier even though the name heuristic
    /// for an unpinned local provider would have said 32,768.
    #[test]
    fn an_engine_reported_3k_jetson_classifies_as_small() {
        let inputs = ContextInputs {
            provider: "local",
            model: "gemma-4-e2b",
            engine_reported: Some(EngineWindow::new("gemma-4-e2b", 3_072)),
            ..Default::default()
        };
        let resolution = ContextGovernor::resolve(&inputs);
        assert_eq!(resolution.source, WindowSource::EngineReported);
        assert_eq!(
            ModelClass::from_resolution(inputs.provider, &resolution),
            ModelClass::Small
        );

        // Without the report the same pond takes the heuristic ceiling and is
        // medium -- which is the point of classifying from the RESOLUTION.
        let bare = ContextInputs {
            provider: "local",
            model: "gemma-4-e2b",
            ..Default::default()
        };
        assert_eq!(
            ModelClass::from_resolution(bare.provider, &ContextGovernor::resolve(&bare)),
            ModelClass::Medium
        );
    }

    /// The whole reason `runs_on_this_device` is not `is_local_provider`: an
    /// on-device provider must not unlock a blocking summarisation call however
    /// big its window is.
    ///
    /// **This test used to reach that state through rung 3, and it was reading a
    /// bug.** It asserted a 131,072-token *catalog* resolution for Ollama and
    /// noted that "rung 3 does not clamp it because Ollama is not a local
    /// provider by the governor's preamble-cost definition". That was true and it
    /// was the defect: the governor's `is_local_provider` covered only
    /// local/gguf, so the declared maximum Ollama reports from `/api/show` went
    /// unbounded, while the same weights through `local` were held to the
    /// ceiling. This phase compensated for it here instead of fixing it there,
    /// which left the tier right and the *window* four times too big for every
    /// budget downstream. Rung 3 now clamps anything that runs on this device.
    ///
    /// So the fixture moved to rung 1. An engine-reported window IS an
    /// allocation and is deliberately never clamped — a big box really can give
    /// Ollama 131,072 — which makes it the honest way to reach a large window on
    /// an on-device provider, and it keeps this test exercising the provider
    /// guard rather than the window bracket. Through rung 3 it would now resolve
    /// to 32,768 and land in Medium on width alone, proving nothing.
    #[test]
    fn a_large_window_on_an_on_device_provider_stays_out_of_the_large_tier() {
        let inputs = ContextInputs {
            provider: "ollama",
            model: "gemma4:e2b",
            engine_reported: Some(EngineWindow::new("gemma4:e2b", 131_072)),
            ..Default::default()
        };
        let resolution = ContextGovernor::resolve(&inputs);
        assert_eq!(resolution.tokens, 131_072, "an allocation is not clamped");
        assert_eq!(resolution.source, WindowSource::EngineReported);
        assert_eq!(
            ModelClass::from_resolution(inputs.provider, &resolution),
            ModelClass::Medium,
            "an Ollama model on this box was handed the tier that spends a model \
             call on compaction"
        );
    }

    /// And the rung-3 half, now that it is bounded: the catalog's declared
    /// maximum no longer escapes the ceiling for an on-device provider.
    #[test]
    fn an_ollama_catalog_maximum_is_bounded_by_the_governor() {
        let inputs = ContextInputs {
            provider: "ollama",
            model: "gemma4:e2b",
            catalog_context_length: Some(131_072),
            ..Default::default()
        };
        let resolution = ContextGovernor::resolve(&inputs);
        assert_eq!(
            resolution.tokens, 32_768,
            "a declared maximum is not an allocation, whoever declared it"
        );
        assert_eq!(resolution.source, WindowSource::CatalogRecord);
    }

    /// And the hosted counterpart, so the test above is not passing because the
    /// large tier is unreachable through the governor at all.
    #[test]
    fn a_hosted_128k_model_reaches_the_large_tier_through_the_governor() {
        let inputs = ContextInputs {
            provider: "anthropic",
            model: "claude-sonnet",
            catalog_context_length: Some(200_000),
            ..Default::default()
        };
        let resolution = ContextGovernor::resolve(&inputs);
        assert_eq!(
            ModelClass::from_resolution(inputs.provider, &resolution),
            ModelClass::Large
        );
    }

    #[test]
    fn every_class_has_a_distinct_label() {
        let labels = [
            ModelClass::Small.label(),
            ModelClass::Medium.label(),
            ModelClass::Large.label(),
        ];
        let mut sorted = labels.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len());
    }
}
