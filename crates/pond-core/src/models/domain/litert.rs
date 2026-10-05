//! LiteRT-LM models: `.litertlm` files the in-process `local` provider runs on goose's `litert`
//! backend. A model's id is its file name, extension included, so no id can be read as a GGUF
//! stem and every routing decision is a suffix test.

use std::path::{Path, PathBuf};

/// File extension of a LiteRT-LM model.
pub const EXTENSION: &str = "litertlm";
/// Directory under `<data_dir>/models/` the files live in.
pub const MODELS_SUBDIR: &str = "litertlm";
/// Overrides the execution backend: `gpu` or `cpu`.
pub const EXECUTION_ENV: &str = "GIAP_LITERT_BACKEND";
/// Overrides multi-token prediction: `on` or `off`.
pub const SPECULATIVE_ENV: &str = "GIAP_LITERT_SPECULATIVE";
/// Engine context off the budgeted device.
pub const PLATFORM_CONTEXT_TOKENS: u32 = 16384;
/// Engine context on the budgeted device, measured on the Orin with E4B on the GPU under the
/// pond's workload: 4096 cannot hold the pond's first prompt (about 5,100 tokens with its 41
/// tools), 16384 ran out of memory, and 8192 ran, with little to spare (about 50 MB free and
/// 1.6 GB in swap at its lowest).
pub const DEVICE_CONTEXT_TOKENS: u32 = 8192;
/// giap-main's switch (87e4a9e7) for the prefill chunk sizes LiteRT-LM compiles, read by the
/// engine at creation. Unset or empty keeps every size the model ships; a list that leaves a model
/// none of its prefill signatures fails engine creation.
pub const PREFILL_SIGNATURES_ENV: &str = "LITERT_PREFILL_SIGNATURES";
/// The only prefill chunk compiled on the budgeted device. Gemma 4 ships 128 and 1024, and on the
/// Orin's WebGPU path each compiled chunk size keeps attention scratch sized by the whole context,
/// which the GPU's pool never gives back. Over eight turns with one compaction (E4B, 8k) the GPU's
/// share of RAM grew from 4.8 to 6.6 GB with both sizes and the kernel swapped 2.2 GB of the pond
/// out; with 128 alone it stayed at 4.1 GB, nothing was swapped, and the first token after the
/// compaction came in 4.1 s instead of 10-16 s.
pub const DEVICE_PREFILL_SIGNATURES: &str = "128";

/// The value to give [`PREFILL_SIGNATURES_ENV`] on this device, or `None` to leave it alone: off
/// the budgeted device, and whenever it is already set, empty included, because an explicit
/// setting wins.
pub fn prefill_signatures(budgeted_device: bool, current: Option<&str>) -> Option<&'static str> {
    (budgeted_device && current.is_none()).then_some(DEVICE_PREFILL_SIGNATURES)
}

/// Whether `model` names a LiteRT-LM model.
pub fn is_litert_model(model: &str) -> bool {
    Path::new(model)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

/// `<data_dir>/models/litertlm`.
pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join(MODELS_SUBDIR)
}

/// Where `model`'s file lives. Only its last path component is used, so no id can name a file
/// outside [`models_dir`].
pub fn model_path(data_dir: &Path, model: &str) -> PathBuf {
    let dir = models_dir(data_dir);
    match Path::new(model).file_name() {
        Some(file) => dir.join(file),
        None => dir,
    }
}

/// Where the engine executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Execution {
    Gpu,
    Cpu,
}

impl Execution {
    /// The value goose's `LiteRtSettings.backend` takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gpu" => Some(Self::Gpu),
            "cpu" => Some(Self::Cpu),
            _ => None,
        }
    }
}

/// What the environment may override; a value it does not recognise overrides nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overrides {
    pub execution: Option<Execution>,
    pub speculative_decoding: Option<bool>,
}

impl Overrides {
    pub fn resolve(lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            execution: lookup(EXECUTION_ENV).and_then(|v| Execution::parse(&v)),
            speculative_decoding: lookup(SPECULATIVE_ENV).and_then(|v| parse_switch(&v)),
        }
    }

    pub fn from_env() -> Self {
        Self::resolve(|key| std::env::var(key).ok())
    }
}

fn parse_switch(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => Some(true),
        "off" | "0" | "false" => Some(false),
        _ => None,
    }
}

/// What a LiteRT-LM engine is created with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOptions {
    /// The engine's `max_num_tokens`, fixed for the engine's life.
    pub context_tokens: u32,
    pub execution: Execution,
    pub speculative_decoding: bool,
}

/// The options `model` gets on the budgeted device or off it: the GPU, and the measured
/// speculation rule, unless overridden.
pub fn engine_options(model: &str, budgeted_device: bool, overrides: Overrides) -> EngineOptions {
    let execution = overrides.execution.unwrap_or(Execution::Gpu);
    EngineOptions {
        context_tokens: if budgeted_device {
            DEVICE_CONTEXT_TOKENS
        } else {
            PLATFORM_CONTEXT_TOKENS
        },
        execution,
        speculative_decoding: overrides
            .speculative_decoding
            .unwrap_or_else(|| !budgeted_device && speculative_decoding(model, execution)),
    }
}

/// Multi-token prediction off the budgeted device: off only where it loses. On the Orin it is off
/// for every model (see [`engine_options`]).
///
/// The Orin's first measurement, before the GPU ran in fp32, favoured it: E4B 1.47x and E2B 1.18x
/// over summaries, code and rewrites, whose drafts copy the prompt (E2B on the CPU 0.95x). Its
/// free-form row already pointed the other way (E2B 0.88x, 22% of drafts accepted; E4B 1.16x,
/// 25%). The GPU now runs fp32, which tool calls need, and rejected drafts cost more there: the
/// pond's own prompts with E4B (2026-10-04) decoded prose at 9.3-9.8 tok/s with it and 14.1-14.2
/// without, finishing a 45-token answer 1.4 s later, while copied text and tool calls finished
/// 4-16% sooner. Household replies are mostly prose. Off the device it has not been measured
/// again since the switch to fp32.
pub fn speculative_decoding(model: &str, execution: Execution) -> bool {
    !(execution == Execution::Cpu && model.to_ascii_lowercase().contains("e2b"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_is_litert_by_its_extension_alone() {
        assert!(is_litert_model("gemma-4-E2B-it.litertlm"));
        assert!(is_litert_model("Gemma-4-E4B-it.LITERTLM"));
        assert!(!is_litert_model("gemma-4-E2B-it"));
        assert!(!is_litert_model("gemma-4-E2B-it-Q4_K_M.gguf"));
        assert!(!is_litert_model("litertlm"));
        assert!(!is_litert_model(""));
    }

    #[test]
    fn a_model_path_stays_inside_the_models_dir() {
        let dd = Path::new("/pond");
        assert_eq!(
            model_path(dd, "gemma-4-E2B-it.litertlm"),
            Path::new("/pond/models/litertlm/gemma-4-E2B-it.litertlm")
        );
        assert_eq!(
            model_path(dd, "../../etc/x.litertlm"),
            Path::new("/pond/models/litertlm/x.litertlm")
        );
        assert_eq!(
            model_path(dd, "/abs/elsewhere/y.litertlm"),
            Path::new("/pond/models/litertlm/y.litertlm")
        );
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn the_budgeted_device_compiles_only_the_short_prefill_chunk_unless_told_otherwise() {
        assert_eq!(prefill_signatures(true, None), Some("128"));
        assert_eq!(prefill_signatures(false, None), None);
        assert_eq!(
            prefill_signatures(true, Some("")),
            None,
            "an empty value asks for every size the model ships"
        );
        assert_eq!(prefill_signatures(true, Some("128,1024")), None);
    }

    #[test]
    fn the_budgeted_device_gets_the_narrower_window() {
        let none = Overrides::default();
        let device = engine_options("gemma-4-E2B-it.litertlm", true, none);
        let platform = engine_options("gemma-4-E2B-it.litertlm", false, none);
        assert_eq!(device.context_tokens, DEVICE_CONTEXT_TOKENS);
        assert_eq!(platform.context_tokens, PLATFORM_CONTEXT_TOKENS);
        assert!(device.context_tokens < platform.context_tokens);
    }

    #[test]
    fn speculation_is_off_only_for_e2b_on_the_cpu() {
        let e2b = "gemma-4-E2B-it.litertlm";
        let e4b = "gemma-4-E4B-it.litertlm";
        assert!(speculative_decoding(e2b, Execution::Gpu));
        assert!(speculative_decoding(e4b, Execution::Gpu));
        assert!(speculative_decoding(e4b, Execution::Cpu));
        assert!(!speculative_decoding(e2b, Execution::Cpu));
        let cpu = Overrides {
            execution: Some(Execution::Cpu),
            ..Overrides::default()
        };
        assert!(!engine_options(e2b, true, cpu).speculative_decoding);
    }

    #[test]
    fn the_budgeted_device_does_not_speculate_unless_told_to() {
        let on = Overrides {
            speculative_decoding: Some(true),
            ..Overrides::default()
        };
        for model in ["gemma-4-E2B-it.litertlm", "gemma-4-E4B-it.litertlm"] {
            assert!(!engine_options(model, true, Overrides::default()).speculative_decoding);
            assert!(engine_options(model, false, Overrides::default()).speculative_decoding);
            assert!(engine_options(model, true, on).speculative_decoding);
        }
    }

    #[test]
    fn the_engine_runs_on_the_gpu_unless_told_otherwise() {
        let options = |pairs| engine_options("m.litertlm", false, Overrides::resolve(env(pairs)));
        assert_eq!(options(&[]).execution, Execution::Gpu);
        assert_eq!(
            options(&[(EXECUTION_ENV, " CPU ")]).execution,
            Execution::Cpu
        );
        assert_eq!(
            options(&[(EXECUTION_ENV, "npu")]).execution,
            Execution::Gpu,
            "an unknown value must not leave the engine without a backend"
        );
        assert_eq!(Execution::Gpu.as_str(), "gpu");
        assert_eq!(Execution::Cpu.as_str(), "cpu");
    }

    #[test]
    fn speculation_can_be_switched_either_way() {
        let e2b = "gemma-4-E2B-it.litertlm";
        let speculates =
            |pairs| engine_options(e2b, false, Overrides::resolve(env(pairs))).speculative_decoding;
        assert_eq!(speculates(&[]), speculative_decoding(e2b, Execution::Gpu));
        assert!(!speculates(&[(SPECULATIVE_ENV, "off")]));
        assert!(speculates(&[
            (EXECUTION_ENV, "cpu"),
            (SPECULATIVE_ENV, "on")
        ]));
        assert_eq!(
            speculates(&[(SPECULATIVE_ENV, "maybe")]),
            speculative_decoding(e2b, Execution::Gpu),
            "an unknown value keeps the rule"
        );
    }
}
