//! The single answer to "how big is this model's context window?"
//!
//! Four code paths used to answer that question independently, and they did not
//! agree. The worst of them was the live history trimmer, which read
//! `GOOSE_CONTEXT_LIMIT` from the process environment and fell back to a
//! hardcoded 8192 — so a Jetson whose engine had actually allocated 4096 could
//! be budgeting history against a window twice the real size, and a Mac that had
//! allocated 32K could be throwing away history it had room for.
//!
//! This module owns the precedence. Callers supply what they know; the governor
//! decides. Nothing here reads the environment: configuration arrives as an
//! argument or it does not arrive at all (see `PAI-3` invariant 5 in
//! `docs/architecture/pai/03-context-governor.md`).
//!
//! # Precedence
//!
//! 1. [`WindowSource::EngineReported`] — what the engine says it actually
//!    allocated. Ground truth, because it is the allocation rather than a
//!    prediction of it. Only accepted when it is tagged with the model it was
//!    measured for (see [`EngineWindow`]).
//! 2. [`WindowSource::Registry`] — a pinned `context_size` in Goose's local
//!    model registry. Authoritative for GGUF because the engine ranks it above
//!    its own memory estimate, which makes it the allocation too.
//! 3. [`WindowSource::CatalogRecord`] — `ModelRecord.context_length`, for
//!    HTTP and Ollama models where no registry entry exists. A declared
//!    maximum rather than an allocation, so it is bounded both by the local
//!    ceiling and by any user override that is LOWER than it.
//! 4. [`WindowSource::Override`] — the user's `context_window_override`. An
//!    escape hatch for deployments whose real limit is neither the model's max
//!    nor the engine's estimate.
//! 5. [`WindowSource::Heuristic`] — last resort, from the model name.
//!
//! Rungs 1 and 2 outrank the user override deliberately. An override is a
//! preference; an allocation is a fact, and budgeting above it only makes the
//! engine truncate. Rung 3 does NOT outrank it in the widening direction: the
//! catalog knows what the model supports, not what this box can afford.

use crate::models::domain::model_capabilities::ModelCapabilities;

/// Generous ceiling for an unpinned local model. The engine constrains the real
/// allocation by its own memory estimate at inference time, so this is an upper
/// bound rather than a promise.
const UNPINNED_LOCAL_CEILING: usize = 32_768;

/// Prompt-side clamp for local providers. See [`ContextGovernor::prompt_window`].
const LOCAL_PROMPT_CLAMP: usize = 8_192;

/// Providers whose preamble is re-prefilled locally on every turn.
fn is_local_provider(provider: &str) -> bool {
    matches!(provider, "local" | "gguf")
}

/// Where a resolved context window came from.
///
/// Carried rather than discarded: "why does this model think it has 4K?" is a
/// question the Models UI should be able to answer without a debugger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSource {
    /// The engine reported its actual allocation.
    EngineReported,
    /// A pinned `context_size` in the local model registry.
    Registry,
    /// `ModelRecord.context_length` from the model catalog.
    CatalogRecord,
    /// The user's `context_window_override`.
    Override,
    /// Derived from the model name.
    Heuristic,
}

impl WindowSource {
    /// A short label for logs and the Models UI.
    pub fn label(&self) -> &'static str {
        match self {
            WindowSource::EngineReported => "engine",
            WindowSource::Registry => "registry",
            WindowSource::CatalogRecord => "catalog",
            WindowSource::Override => "override",
            WindowSource::Heuristic => "heuristic",
        }
    }

    /// Whether this source reflects a real allocation rather than a prediction.
    ///
    /// Budget code can trust an exact window to the token; an inexact one should
    /// keep the overshoot-feedback safety net that `turn_trimmer` applies.
    pub fn is_exact(&self) -> bool {
        matches!(self, WindowSource::EngineReported | WindowSource::Registry)
    }
}

/// A context window the engine reported, tagged with the model it was measured
/// for.
///
/// The tag is not decoration. `TurnStats.context_limit_tokens` is recorded per
/// turn, so after a model swap the most recent value describes the *previous*
/// model. Feeding that into the next turn's budget is exactly the kind of
/// silent, occasional over-budgeting this module exists to remove, so the
/// governor discards a reading whose model does not match the active one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineWindow {
    pub tokens: u32,
    pub model: String,
}

impl EngineWindow {
    pub fn new(model: impl Into<String>, tokens: u32) -> Self {
        Self {
            tokens,
            model: model.into(),
        }
    }
}

/// Everything the governor needs to resolve a window.
///
/// Fields a caller cannot answer are `None`; the governor falls through. A
/// caller that knows nothing at all still gets a defensible number from the
/// name heuristic.
#[derive(Debug, Clone, Default)]
pub struct ContextInputs<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    /// `Settings.context_window_override`. Zero means unset.
    pub override_tokens: u32,
    /// A pinned `context_size` from the local model registry, when the adapter
    /// could reach it.
    pub registry_pinned: Option<usize>,
    /// `ModelRecord.context_length` for catalog models.
    pub catalog_context_length: Option<u32>,
    /// The engine's own report from a previous turn of this session.
    pub engine_reported: Option<EngineWindow>,
    /// A live capability-reported window, when the caller holds one.
    ///
    /// Used at the heuristic rung in place of re-deriving from the model name.
    /// It is the same *kind* of answer — a declared window rather than an
    /// allocation — but a better-informed one, since the adapter populates
    /// capabilities from the active model rather than from a substring match.
    pub capability_window: Option<u32>,
}

/// A resolved window and its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowResolution {
    pub tokens: usize,
    pub source: WindowSource,
}

impl WindowResolution {
    /// Whether the underlying source reflects a real allocation.
    pub fn is_exact(&self) -> bool {
        self.source.is_exact()
    }
}

/// Resolves context windows. Stateless: every answer is a pure function of its
/// inputs, which is what makes the precedence testable without a live engine.
pub struct ContextGovernor;

impl ContextGovernor {
    /// Resolve the context window, with provenance.
    pub fn resolve(inputs: &ContextInputs<'_>) -> WindowResolution {
        // 1. The engine's own report, if it describes the model we are about to
        //    run. A zero is meaningless and is treated as absent.
        if let Some(engine) = &inputs.engine_reported {
            if engine.tokens > 0 && engine.model == inputs.model {
                return WindowResolution {
                    tokens: engine.tokens as usize,
                    source: WindowSource::EngineReported,
                };
            }
        }

        // 2. A pinned registry size IS the allocation, so it outranks the
        //    override for the same reason the engine ranks it that way.
        if let Some(pinned) = inputs.registry_pinned.filter(|p| *p > 0) {
            return WindowResolution {
                tokens: pinned,
                source: WindowSource::Registry,
            };
        }

        // 3. Catalog metadata, for models with no registry entry.
        //
        // A catalog `context_length` is the model's DECLARED maximum, not an
        // allocation, so for a local provider it is clamped by the same
        // ceiling the heuristic rung applies. Gemma 4 declares 131,072
        // (verified against Ollama's `model_info`); an unpinned Mac that
        // allocated 32K would otherwise budget history for four times the
        // room the engine has, and the engine answers that by truncating the
        // prompt. Rungs 1 and 2 are allocations and are never clamped.
        //
        // A user override is the same KIND of bound as the local ceiling, only
        // stated by hand, so it clamps this rung too -- see
        // `a_lower_override_bounds_the_catalog_maximum`. Rungs 1 and 2 still
        // outrank it, because those are allocations (invariant 3).
        if let Some(catalog) = inputs.catalog_context_length.filter(|c| *c > 0) {
            let mut tokens = catalog as usize;
            // `runs_on_this_device`, NOT `is_local_provider`. The question this
            // rung asks is "can this box afford the number the catalog printed",
            // and that is about where the weights run, not which provider string
            // named them. `is_local_provider` covers only local/gguf, which left
            // ollama and llamafile -- the two providers this rung exists for --
            // taking a declared maximum unbounded.
            //
            // It mattered the moment P3a made the rung reachable: it taught
            // OllamaCatalogProvider to read the declared window from /api/show,
            // where a Gemma 4 model reports 131072. The same weights through the
            // `local` provider were held to 32768, so the history budget was 4x
            // apart depending only on which string arrived here.
            //
            // The other two `is_local_provider` call sites are deliberately left
            // alone. `heuristic_window` answers a different question (what to
            // guess when nothing is known) and `prompt_window` a third (how much
            // preamble a locally-prefilled turn can afford). Widening those is a
            // behaviour change on every Ollama turn and wants its own phase and
            // its own TTFT measurement, not a ride along with a clamp fix.
            if super::model_class::runs_on_this_device(inputs.provider) {
                tokens = tokens.min(UNPINNED_LOCAL_CEILING);
            }
            let override_tokens = inputs.override_tokens as usize;
            if override_tokens > 0 && override_tokens < tokens {
                return WindowResolution {
                    tokens: override_tokens,
                    source: WindowSource::Override,
                };
            }
            return WindowResolution {
                tokens,
                source: WindowSource::CatalogRecord,
            };
        }

        // 4. The user's escape hatch.
        if inputs.override_tokens > 0 {
            return WindowResolution {
                tokens: inputs.override_tokens as usize,
                source: WindowSource::Override,
            };
        }

        // 5. Heuristic. A caller-supplied capability window beats re-deriving
        //    from the model name; otherwise fall back to the name. This is the
        //    only rung that may return the conservative 4096 default.
        let tokens = inputs
            .capability_window
            .filter(|c| *c > 0)
            .map(|c| c as usize)
            .unwrap_or_else(|| Self::heuristic_window(inputs.provider, inputs.model));
        WindowResolution {
            tokens,
            source: WindowSource::Heuristic,
        }
    }

    /// The name-derived fallback.
    ///
    /// Local models get a generous ceiling because the engine narrows it later
    /// from its memory estimate; HTTP providers get the model's declared window.
    fn heuristic_window(provider: &str, model: &str) -> usize {
        if is_local_provider(provider) {
            UNPINNED_LOCAL_CEILING
        } else {
            ModelCapabilities::from_model_name(model).context_window_tokens as usize
        }
    }

    /// The window that PROMPT-side budgets (system prompt tier, memory
    /// injection) should be derived from — as opposed to history budgets, which
    /// use the full resolved window.
    ///
    /// This is the asymmetry that makes "use the window to the fullest" safe on
    /// this hardware. For local in-process inference every preamble token is
    /// re-prefilled on every turn, so a bigger window must buy HISTORY room, not
    /// a more verbose preamble: an unclamped 32K profile on the Mac selected the
    /// full template tier plus a 1.5K memory budget and produced a 9.4K-token
    /// prompt (~17 s TTFT) for a one-line question. HTTP providers keep the raw
    /// window — their preamble is not paid for in local prefill.
    pub fn prompt_window(provider: &str, resolved: usize) -> usize {
        if is_local_provider(provider) {
            resolved.min(LOCAL_PROMPT_CLAMP)
        } else {
            resolved
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(provider: &'a str, model: &'a str) -> ContextInputs<'a> {
        ContextInputs {
            provider,
            model,
            ..Default::default()
        }
    }

    #[test]
    fn engine_report_outranks_everything() {
        let mut i = inputs("local", "gemma-4-e2b");
        i.registry_pinned = Some(4096);
        i.catalog_context_length = Some(8192);
        i.override_tokens = 16384;
        i.engine_reported = Some(EngineWindow::new("gemma-4-e2b", 3072));

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 3072);
        assert_eq!(r.source, WindowSource::EngineReported);
    }

    #[test]
    fn engine_report_for_a_different_model_is_discarded() {
        // The stale-reading guard: TurnStats is recorded per turn, so after a
        // model swap the newest reading describes the model we just left.
        let mut i = inputs("local", "qwen3-4b");
        i.registry_pinned = Some(4096);
        i.engine_reported = Some(EngineWindow::new("gemma-4-e2b", 3072));

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 4096);
        assert_eq!(r.source, WindowSource::Registry);
    }

    #[test]
    fn a_zero_engine_report_is_treated_as_absent() {
        let mut i = inputs("local", "gemma-4-e2b");
        i.registry_pinned = Some(4096);
        i.engine_reported = Some(EngineWindow::new("gemma-4-e2b", 0));

        assert_eq!(ContextGovernor::resolve(&i).source, WindowSource::Registry);
    }

    #[test]
    fn registry_outranks_the_user_override() {
        // Invariant 3: the registry value is the allocation, not a preference.
        // Budgeting above it only makes the engine truncate.
        let mut i = inputs("local", "gemma-4-e2b");
        i.registry_pinned = Some(4096);
        i.override_tokens = 32768;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 4096);
        assert_eq!(r.source, WindowSource::Registry);
    }

    #[test]
    fn catalog_length_is_used_when_no_registry_entry_exists() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.catalog_context_length = Some(16384);

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 16384);
        assert_eq!(r.source, WindowSource::CatalogRecord);
    }

    #[test]
    fn a_lower_override_bounds_the_catalog_maximum() {
        // Corrected when PAI-3 P3b made rung 3 reachable. The documented
        // precedence put CatalogRecord above Override, and while rung 3 was
        // dead that cost nothing. Live, it inverts the one case the override
        // exists for, named in `goose_agent.rs`'s own doc comment: a Jetson
        // running Ollama with a hand-tuned KV cache. Populating the catalog
        // would have replaced that user's 8192 with gemma4's declared 131072
        // and the engine would have truncated every prompt.
        //
        // Rungs 1 and 2 still outrank the override -- they are allocations
        // (invariant 3). A catalog value is not; it says what the MODEL
        // supports, not what this box allocated.
        let mut i = inputs("ollama", "gemma4:e2b");
        i.catalog_context_length = Some(131_072);
        i.override_tokens = 8_192;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 8_192);
        assert_eq!(
            r.source,
            WindowSource::Override,
            "provenance must name the value that actually won"
        );

        // The override cannot WIDEN past the declared maximum: asking for more
        // than the model supports is not an allocation either.
        let mut wide = inputs("ollama", "gemma4:e2b");
        wide.catalog_context_length = Some(16_384);
        wide.override_tokens = 65_536;
        let w = ContextGovernor::resolve(&wide);
        assert_eq!(w.tokens, 16_384);
        assert_eq!(w.source, WindowSource::CatalogRecord);

        // On a local provider the override is compared against the ALREADY
        // ceiling-clamped value, so an override between the ceiling and the
        // declared maximum does not resurrect the 128K window.
        let mut local = inputs("local", "gemma-4-e2b");
        local.catalog_context_length = Some(131_072);
        local.override_tokens = 65_536;
        let l = ContextGovernor::resolve(&local);
        assert_eq!(l.tokens, UNPINNED_LOCAL_CEILING);
        assert_eq!(l.source, WindowSource::CatalogRecord);

        // And a registry pin still beats both, unchanged.
        let mut pinned = inputs("local", "gemma-4-e2b");
        pinned.catalog_context_length = Some(131_072);
        pinned.override_tokens = 8_192;
        pinned.registry_pinned = Some(4_096);
        let p = ContextGovernor::resolve(&pinned);
        assert_eq!(p.tokens, 4_096);
        assert_eq!(p.source, WindowSource::Registry);
    }

    #[test]
    fn a_catalog_length_cannot_widen_an_unpinned_local_window() {
        // The catalog carries the model's DECLARED maximum. Gemma 4 declares
        // 131,072; the engine on an unpinned local install allocates from its
        // own memory estimate, and the heuristic rung caps that expectation at
        // UNPINNED_LOCAL_CEILING. Rung 3 must not be the one rung that escapes
        // it, or populating the catalog silently hands the trimmer a 128K
        // history budget on a machine with 32K.
        let mut i = inputs("local", "gemma-4-e2b");
        i.catalog_context_length = Some(131_072);

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, UNPINNED_LOCAL_CEILING);
        assert_eq!(r.source, WindowSource::CatalogRecord);

        // gguf is the same class of provider.
        let mut g = inputs("gguf", "gemma-4-e2b");
        g.catalog_context_length = Some(131_072);
        assert_eq!(ContextGovernor::resolve(&g).tokens, UNPINNED_LOCAL_CEILING);

        // Below the ceiling it passes through untouched, so a genuinely small
        // model is not inflated to the ceiling.
        let mut small = inputs("local", "gemma-2-2b-it");
        small.catalog_context_length = Some(8_192);
        assert_eq!(ContextGovernor::resolve(&small).tokens, 8_192);

        // Ollama is NOT the exception, and this assertion used to say it was.
        //
        // It read "HTTP providers pay no local prefill, so they keep the raw
        // declared window", with `ollama` as the fixture. Ollama serves over
        // HTTP and runs on this box -- `model_class::ON_DEVICE_PROVIDERS` lists
        // it alongside local, gguf and llamafile -- so it pays the prefill in
        // full. Believing otherwise let a Gemma 4 model on the Orin take the
        // 131,072 its /api/show declares, four times the ceiling the same
        // weights get through the `local` provider, decided by nothing but
        // which string arrived here. The test asserted the defect, so it passed
        // throughout.
        let mut ollama = inputs("ollama", "gemma4:e2b");
        ollama.catalog_context_length = Some(131_072);
        let o = ContextGovernor::resolve(&ollama);
        assert_eq!(
            o.tokens, UNPINNED_LOCAL_CEILING,
            "ollama runs on this device, so a declared maximum is still bounded"
        );
        assert_eq!(o.source, WindowSource::CatalogRecord);

        let mut llamafile = inputs("llamafile", "gemma4-e2b");
        llamafile.catalog_context_length = Some(131_072);
        assert_eq!(
            ContextGovernor::resolve(&llamafile).tokens,
            UNPINNED_LOCAL_CEILING
        );

        // A genuinely hosted provider is the case rung 3 exists for: nothing on
        // this box prefills it, so the declared window stands.
        let mut hosted = inputs("openai", "gpt-4o");
        hosted.catalog_context_length = Some(131_072);
        let h = ContextGovernor::resolve(&hosted);
        assert_eq!(h.tokens, 131_072);
        assert_eq!(h.source, WindowSource::CatalogRecord);
    }

    #[test]
    fn override_wins_when_nothing_authoritative_is_known() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.override_tokens = 2048;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 2048);
        assert_eq!(r.source, WindowSource::Override);
    }

    #[test]
    fn heuristic_is_the_last_resort() {
        let r = ContextGovernor::resolve(&inputs("ollama", "some-unknown-model"));
        assert_eq!(r.source, WindowSource::Heuristic);
        // The conservative default from ModelCapabilities.
        assert_eq!(r.tokens, 4096);
    }

    #[test]
    fn unpinned_local_gets_the_generous_ceiling_not_the_conservative_default() {
        // This is the pre-existing adapter behaviour and it must survive: the
        // engine narrows the real allocation from its memory estimate later.
        let r = ContextGovernor::resolve(&inputs("local", "some-unknown-model"));
        assert_eq!(r.tokens, UNPINNED_LOCAL_CEILING);
        assert_eq!(r.source, WindowSource::Heuristic);
        assert_eq!(
            ContextGovernor::resolve(&inputs("gguf", "some-unknown-model")).tokens,
            UNPINNED_LOCAL_CEILING
        );
    }

    #[test]
    fn zero_valued_authoritative_inputs_fall_through() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.registry_pinned = Some(0);
        i.catalog_context_length = Some(0);
        i.override_tokens = 5000;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 5000);
        assert_eq!(r.source, WindowSource::Override);
    }

    #[test]
    fn a_capability_window_is_preferred_over_the_name_heuristic() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.capability_window = Some(16384);

        let r = ContextGovernor::resolve(&i);
        // Still the heuristic RUNG — it is a declared window, not an
        // allocation — but a better-informed answer than 4096 from the name.
        assert_eq!(r.tokens, 16384);
        assert_eq!(r.source, WindowSource::Heuristic);
        assert!(!r.is_exact());
    }

    #[test]
    fn a_capability_window_still_loses_to_the_override() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.capability_window = Some(16384);
        i.override_tokens = 2048;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 2048);
        assert_eq!(r.source, WindowSource::Override);
    }

    #[test]
    fn a_zero_capability_window_falls_back_to_the_name() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.capability_window = Some(0);

        assert_eq!(ContextGovernor::resolve(&i).tokens, 4096);
    }

    #[test]
    fn prompt_window_clamps_local_providers_only() {
        assert_eq!(ContextGovernor::prompt_window("local", 32768), 8192);
        assert_eq!(ContextGovernor::prompt_window("gguf", 32768), 8192);
        // Already below the clamp: unchanged, so a 3K Jetson keeps 3K.
        assert_eq!(ContextGovernor::prompt_window("local", 3072), 3072);
        // HTTP providers keep the raw window.
        assert_eq!(ContextGovernor::prompt_window("ollama", 32768), 32768);
    }

    #[test]
    fn exactness_tracks_whether_the_source_is_an_allocation() {
        assert!(WindowSource::EngineReported.is_exact());
        assert!(WindowSource::Registry.is_exact());
        assert!(!WindowSource::CatalogRecord.is_exact());
        assert!(!WindowSource::Override.is_exact());
        assert!(!WindowSource::Heuristic.is_exact());
    }

    #[test]
    fn every_source_has_a_distinct_label() {
        let labels = [
            WindowSource::EngineReported.label(),
            WindowSource::Registry.label(),
            WindowSource::CatalogRecord.label(),
            WindowSource::Override.label(),
            WindowSource::Heuristic.label(),
        ];
        let mut sorted = labels.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len());
    }

    /// The governor must never read process environment variables — that is the
    /// bug it exists to remove (PAI-3 invariant 5).
    #[test]
    fn this_module_does_not_read_the_environment() {
        let src = include_str!("context_governor.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        assert!(
            !body.contains("env::var") && !body.contains("std::env"),
            "context_governor must not read process environment variables"
        );
    }
}
