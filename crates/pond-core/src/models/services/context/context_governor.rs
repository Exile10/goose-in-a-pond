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
//!    HTTP and Ollama models where no registry entry exists.
//! 4. [`WindowSource::Override`] — the user's `context_window_override`. An
//!    escape hatch for deployments whose real limit is neither the model's max
//!    nor the engine's estimate.
//! 5. [`WindowSource::Heuristic`] — last resort, from the model name.
//!
//! Rungs 1 and 2 outrank the user override deliberately. An override is a
//! preference; an allocation is a fact, and budgeting above it only makes the
//! engine truncate.

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
        if let Some(catalog) = inputs.catalog_context_length.filter(|c| *c > 0) {
            return WindowResolution {
                tokens: catalog as usize,
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
        i.override_tokens = 2048;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 16384);
        assert_eq!(r.source, WindowSource::CatalogRecord);
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
