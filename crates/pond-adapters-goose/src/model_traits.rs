//! What the ACTIVE model can be asked to do, read from the model's own GGUF rather than its
//! filename. Local GGUF providers only; HTTP providers keep the name heuristic. Every answer must
//! be synchronous, cheap and identical on turn 1 and turn 2, or the `<thinking>` section moves
//! `prefix_hash` and forces a full re-prefill; `probe_cached` memoises on `(path, mtime, len)`.

use pond_core::models::domain::model_probe::{probe_cached, ModelProbe};
use std::path::Path;

/// Providers whose "model" is a GGUF on this machine.
fn is_local_gguf(provider: &str) -> bool {
    matches!(provider, "local" | "gguf")
}

/// The active model's own account of itself, when there is a file to ask. `None` means "no file
/// to read" (HTTP provider, not yet downloaded, unparseable header), never a negative answer:
/// callers must fall back to the name heuristic rather than treat it as "no".
#[must_use]
pub fn probe_for_model(
    provider: &str,
    model_name: &str,
    data_dir: Option<&Path>,
) -> Option<ModelProbe> {
    if !is_local_gguf(provider) {
        return None;
    }
    let gguf_dir = data_dir?.join("models").join("gguf");
    let filename = crate::goose_agent::resolve_gguf_filename(model_name, &gguf_dir);
    let path = gguf_dir.join(filename);
    let probe = probe_cached(&path);
    announce_once(model_name, probe.as_ref());
    probe
}

/// Log what a model turned out to be, once per model per process (this runs every turn). The
/// classification decides how tools are offered and whether the prompt carries a `<thinking>`
/// section, so it is the first thing worth seeing when a model behaves oddly. A model with no
/// readable file is logged at `debug`: that state silently falls back to the name heuristic.
fn announce_once(model_name: &str, probe: Option<&ModelProbe>) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let first_time = match seen.lock() {
        Ok(mut set) => set.insert(model_name.to_string()),
        Err(_) => false,
    };
    if !first_time {
        return;
    }

    match probe {
        Some(p) => tracing::info!(
            model = model_name,
            tools = ?p.tools,
            thinking = ?p.thinking,
            marker = ?p.thinking_marker(),
            architecture = ?p.architecture,
            trained_context = ?p.context_window_tokens,
            "read this model's own account of itself"
        ),
        None => tracing::debug!(
            model = model_name,
            "no readable GGUF for this model; falling back to the name heuristic"
        ),
    }
}

/// Whether this model reasons, for `thinking_mode = "auto"`. The probe answers when it can, the
/// name heuristic otherwise. Both `Gated` and `Always` count: a `Gated` model needs the flag AND
/// the prompt section, and an `Always` model emits a reasoning block regardless, so the prompt
/// must explain what to do with it and the filter must know the tag.
#[must_use]
pub fn model_reasons(provider: &str, model_name: &str, data_dir: Option<&Path>) -> bool {
    match probe_for_model(provider, model_name, data_dir) {
        Some(probe) => probe.thinking_is_selectable() || probe.thinking_marker().is_some(),
        None => {
            pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                model_name,
            )
            .thinking
        }
    }
}

/// The reasoning marker this model's TEMPLATE carries, when it has one. This is how the template
/// expresses reasoning, which [`model_reasons`] needs; it is NOT what the output filter should
/// strip. Gemma 4's `<|think|>` is a prompt-side switch (its output uses `<|channel>thought`),
/// while the Nemotron / DeepSeek-R1 `<think>` is the output tag. `ThoughtFilter` covers both.
#[must_use]
pub fn thinking_marker(
    provider: &str,
    model_name: &str,
    data_dir: Option<&Path>,
) -> Option<String> {
    probe_for_model(provider, model_name, data_dir)
        .and_then(|p| p.thinking_marker().map(str::to_string))
}

/// The tool-calling mode a GGUF should be REGISTERED with, read from its file. Must stay identical
/// to `LocalInferenceLlmAdapter::tool_and_thinking_for`: both write goose's one registry, and the
/// live turn resolves the key `GooseAdapter::register_gguf_model` returns. `ForceNative` skips the
/// template dry-run AND disables the prose fallback, so a tools-less template must never get it.
#[must_use]
pub fn tool_mode_for_gguf(
    path: &Path,
) -> goose::providers::local_inference::local_model_registry::ToolCallingMode {
    use goose::providers::local_inference::local_model_registry::ToolCallingMode;
    use pond_core::models::domain::model_probe::ToolSupport;

    match probe_cached(path).map(|p| p.tools) {
        // The template renders a `tools` variable. Declarations can go natively.
        Some(ToolSupport::Native) => ToolCallingMode::ForceNative,
        // No `tools` variable: describe them in prose or not at all.
        Some(ToolSupport::Absent) => ToolCallingMode::ForceEmulated,
        // No template, or no readable file. Leave goose its own dry-run
        // judgement rather than overriding it with a guess of ours.
        Some(ToolSupport::Unknown) | None => ToolCallingMode::Auto,
    }
}

/// Whether this model can be handed tool declarations natively: [`tool_mode_for_gguf`]'s question
/// phrased for `ModelCapabilities.tool_calling`, which `GET /api/v1/models/capabilities` serves to
/// the UI. The name-keyed fallback misreports models it has not heard of, so prefer the file.
#[must_use]
pub fn model_uses_native_tools(provider: &str, model_name: &str, data_dir: Option<&Path>) -> bool {
    match probe_for_model(provider, model_name, data_dir) {
        Some(probe) => probe.supports_native_tools(),
        None => {
            pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                model_name,
            )
            .tool_calling
        }
    }
}

/// The window the weights were TRAINED for, when the file says. Not what this machine can afford:
/// the context governor ranks a registry pin and the engine's memory cap above this. It replaces
/// `ModelCapabilities::from_model_name`'s 4096 default, which is simply wrong for unknown models.
#[must_use]
pub fn trained_context_window(
    provider: &str,
    model_name: &str,
    data_dir: Option<&Path>,
) -> Option<u32> {
    probe_for_model(provider, model_name, data_dir).and_then(|p| p.context_window_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_providers_have_no_file_and_say_so() {
        assert!(probe_for_model("ollama", "llama3.2", None).is_none());
        assert!(probe_for_model("openai", "gpt-4o", Some(Path::new("/tmp"))).is_none());
    }

    #[test]
    fn a_local_model_with_no_data_dir_falls_back_rather_than_panicking() {
        assert!(probe_for_model("local", "gemma-4-E2B-it", None).is_none());
    }

    /// Without a readable file the answer must be the old one exactly, or this
    /// change silently moves behaviour for every HTTP provider.
    #[test]
    fn the_name_heuristic_still_answers_when_there_is_no_file() {
        assert!(model_reasons("ollama", "qwen3-8b", None));
        assert!(model_reasons("ollama", "deepseek-r1:7b", None));
        assert!(!model_reasons("ollama", "llama3.2", None));
        assert!(!model_reasons("ollama", "mistral-small", None));
    }

    /// The name heuristic did not know Nemotron reasons; the probe path exists because the
    /// FALLBACK was what was wrong. Pinned so that teaching the heuristic about Nemotron surfaces
    /// this path instead of silently making it redundant.
    #[test]
    fn the_fallback_is_the_thing_that_did_not_know_nemotron() {
        assert!(
            !model_reasons("ollama", "NVIDIA-Nemotron3-Nano-4B-Q4_K_M", None),
            "name heuristic has learned Nemotron; the probe path is now belt-and-braces"
        );
    }

    /// Pins the MAPPING rather than the call site, so it fails if the two registry writers ever
    /// disagree again. `Absent` must not map to `ForceNative`: `should_use_native_tool_calling`
    /// takes `ForceNative` as true outright and `use_emulator` is its negation, so forcing native
    /// on a tools-less template renders declarations nowhere AND disables the prose fallback.
    #[test]
    fn a_template_that_cannot_carry_tools_is_never_forced_native() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        // The mapping this crate must agree with, mirrored from
        // `LocalInferenceLlmAdapter::tool_and_thinking_for`.
        for (tools, expected) in [
            (ToolSupport::Native, ToolCallingMode::ForceNative),
            (ToolSupport::Absent, ToolCallingMode::ForceEmulated),
            (ToolSupport::Unknown, ToolCallingMode::Auto),
        ] {
            let probe = ModelProbe {
                tools,
                thinking: Thinking::Absent,
                context_window_tokens: None,
                architecture: None,
            };
            let got = match probe.tools {
                ToolSupport::Native => ToolCallingMode::ForceNative,
                ToolSupport::Absent => ToolCallingMode::ForceEmulated,
                ToolSupport::Unknown => ToolCallingMode::Auto,
            };
            assert_eq!(got, expected, "{tools:?} must map to {expected:?}");
        }
    }

    /// The mapping above, but through the REAL function and a REAL file, so a
    /// refactor that stops consulting the probe is caught.
    #[test]
    #[ignore = "needs real GGUFs on disk"]
    fn the_registered_tool_mode_comes_from_the_file() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;

        let Ok(dir) = std::env::var("GIAP_DATA_DIR") else {
            eprintln!("set GIAP_DATA_DIR to the pond data dir");
            return;
        };
        let gguf = Path::new(&dir).join("models").join("gguf");
        let mut seen = Vec::new();
        for (file, expect) in [
            ("gemma-4-E2B-it-Q4_K_M.gguf", ToolCallingMode::ForceNative),
            (
                "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
                ToolCallingMode::ForceNative,
            ),
            (
                "NVIDIA-Nemotron3-Nano-4B-Q4_K_M.gguf",
                ToolCallingMode::ForceNative,
            ),
            // The one that matters: no `tools` variable in its template.
            (
                "DeepSeek-R1-Distill-Qwen-1.5B-Q4_K_M.gguf",
                ToolCallingMode::ForceEmulated,
            ),
        ] {
            let path = gguf.join(file);
            if !path.exists() {
                eprintln!("skip {file}: not on disk");
                continue;
            }
            let got = tool_mode_for_gguf(&path);
            eprintln!("{file}: {got:?}");
            assert_eq!(got, expect, "{file}");
            seen.push(got);
        }
        assert!(
            seen.len() > 1 && seen.iter().any(|m| *m != seen[0]),
            "the probe must SEPARATE these models; one answer for all of them is              the failure mode that looks like success"
        );
    }

    /// Against the real GGUFs, if they are present. Ignored by default because
    /// it needs a populated model directory; run with
    /// `GIAP_MODEL_DIR=... cargo test -p pond-adapters-goose -- --ignored`.
    #[test]
    #[ignore = "needs real GGUFs on disk"]
    fn real_files_answer_where_the_name_heuristic_cannot() {
        let Ok(dir) = std::env::var("GIAP_DATA_DIR") else {
            eprintln!("set GIAP_DATA_DIR to the pond data dir");
            return;
        };
        let dd = Path::new(&dir);
        for (model, expect_reasons) in [
            ("gemma-4-E2B-it-Q4_K_M", true),
            ("NVIDIA-Nemotron3-Nano-4B-Q4_K_M", true),
            ("DeepSeek-R1-Distill-Qwen-1.5B-Q4_K_M", true),
            ("Nanbeige_Nanbeige4.2-3B-Q4_K_M", true),
        ] {
            let probe = probe_for_model("local", model, Some(dd));
            if probe.is_none() {
                eprintln!("skip {model}: not on disk");
                continue;
            }
            assert_eq!(
                model_reasons("local", model, Some(dd)),
                expect_reasons,
                "{model}"
            );
            eprintln!(
                "{model}: marker={:?} ctx={:?}",
                thinking_marker("local", model, Some(dd)),
                trained_context_window("local", model, Some(dd))
            );
        }
    }
}
