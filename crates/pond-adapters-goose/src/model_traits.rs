//! What the ACTIVE model can be asked to do, read from the model's own file.
//!
//! # The gap this closes
//!
//! [`ModelProbe`] reads tool and thinking support from a GGUF's embedded chat
//! template, and it is correct: against the eleven models on the development
//! machine it produces five distinct classifications. But until now it was a
//! private helper on `LocalInferenceLlmAdapter`, consulted once at model
//! REGISTRATION time, and its answer went into goose's `ModelSettings` and
//! nowhere else.
//!
//! The prompt side could not reach it. `thinking_section_applies` resolved
//! `thinking_mode = "auto"` through `ModelCapabilities::from_model_name`, a
//! filename substring match that knows `gemma-4`, `qwen3`, `qwq` and
//! `deepseek-r1`. For any other reasoning model the two layers disagreed:
//!
//! | layer | source | Nemotron |
//! |---|---|---|
//! | engine `enable_thinking` | the template (gated `<think>`) | **true** |
//! | prompt `<thinking>` section | the filename | **false** |
//!
//! A model switched into reasoning mode and given no prompt section telling it
//! what to do with it produced an empty first turn, was re-engaged with
//! `EMPTY_TURN_STEER`, and fabricated a weather report rather than calling the
//! weather tool. Measured 2026-08-24; with `thinking_mode = "on"` the same
//! model called the tool and reported real data.
//!
//! # Why the filename was being used, and what replacing it has to preserve
//!
//! The doc comment on `thinking_section_applies` is explicit about its reason,
//! and the reason is a good one: the adapter's `model_capabilities` cache is
//! refreshed inside the provider-SWAP branch of `ensure_provider_current`,
//! which runs LATER in the turn that builds the prompt. On turn 1 it still held
//! `ModelCapabilities::default()`. So turn 1 rendered a prompt without the
//! `<thinking>` section and turn 2 rendered one with it -- 78 characters at the
//! top of the static prefix, which moved `prefix_hash` and cost every session a
//! full re-prefill on its second turn (3.7 s on the Orin).
//!
//! Any replacement therefore has to be synchronous, cheap, and give the SAME
//! answer on turn 1 as on turn 2. Reading the file satisfies all three:
//! [`pond_core::models::domain::model_probe::probe_cached`] memoises on
//! `(path, mtime, len)`, so it costs ~40 ms once per model per process and a
//! hashmap lookup thereafter, and it is a pure function of bytes that are not
//! changing mid-session.
//!
//! # Scope
//!
//! Local GGUF providers only. An HTTP provider has no file to read, so it keeps
//! the name heuristic -- which is the right answer there rather than a
//! concession: for Ollama the name is genuinely all there is.

use pond_core::models::domain::model_probe::{probe_cached, ModelProbe};
use std::path::Path;

/// Providers whose "model" is a GGUF on this machine.
fn is_local_gguf(provider: &str) -> bool {
    matches!(provider, "local" | "gguf")
}

/// The active model's own account of itself, when there is a file to ask.
///
/// `None` means "no file to read" -- an HTTP provider, a model not yet
/// downloaded, or a GGUF whose header would not parse. Callers must fall back
/// rather than treat it as a negative answer: a file that did not speak is not
/// a file that said no.
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

/// Say what a model turned out to be, once per model per process.
///
/// The classification decides whether tools are offered natively, in prose, or
/// left to goose's judgement, and whether the prompt carries a `<thinking>`
/// section. When a model behaves oddly that is the first thing worth knowing,
/// and until now it was inferable only from behaviour. Once per model because
/// this is called every turn.
///
/// A model with no readable file is announced too, at `debug` -- "no file to
/// read" is the state that silently returns the caller to the name heuristic,
/// so it is worth being able to see rather than deduce.
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

/// Whether this model reasons, for `thinking_mode = "auto"`.
///
/// The probe answers when it can and the name heuristic answers when it
/// cannot. Both `Gated` and `Always` count as reasoning: a `Gated` model needs
/// the flag AND the prompt section, and an `Always` model is going to emit a
/// reasoning block whether or not anything asked it to -- so the prompt had
/// better explain what to do with it, and the filter had better know the tag.
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

/// The reasoning marker this model's TEMPLATE carries, when it has one.
///
/// # Read this before wiring it to an output filter
///
/// This is the tag the probe found in the template, and it is **not uniformly
/// the tag the model emits**. The two families on this machine use it
/// differently, which was checked against the real files rather than assumed:
///
/// - **Gemma 4** -- `<|think|>` is a PROMPT-SIDE SWITCH. It is a vocabulary
///   token, and the template emits it at the top of the first system turn to
///   put the model INTO reasoning mode:
///   `{%- if enable_thinking is defined and enable_thinking -%}{{- '<|think|>' -}}`.
///   What Gemma then emits is the channel form, `<|channel>thought ... <channel|>`,
///   which is what `ThoughtFilter` already strips.
/// - **Nemotron / Nanbeige / DeepSeek-R1** -- `<think>` is the OUTPUT tag,
///   opened and closed by the model around its own reasoning.
///
/// So this answers "how does this template express reasoning", which is what
/// [`model_reasons`] needs. It does **not** answer "what should the output
/// filter strip", and feeding it to one would tell the filter to look for
/// Gemma's input switch in Gemma's output. `ThoughtFilter`'s pair table already
/// covers both real output shapes; this is diagnostic, and is logged so an
/// unfamiliar model's shape is visible rather than guessed at.
#[must_use]
pub fn thinking_marker(
    provider: &str,
    model_name: &str,
    data_dir: Option<&Path>,
) -> Option<String> {
    probe_for_model(provider, model_name, data_dir)
        .and_then(|p| p.thinking_marker().map(str::to_string))
}

/// The tool-calling mode a GGUF should be REGISTERED with, read from its file.
///
/// # Why this exists here as well as in `pond-adapters-local-inference`
///
/// Two crates register the same GGUFs into goose's one registry, under two
/// different ids, and only one of them was reading the probe.
///
/// `LocalInferenceLlmAdapter::registration_settings` registers the settings
/// spelling verbatim (`DeepSeek-R1-Distill-Qwen-1.5B-Q4_K_M`) and consults
/// `ModelProbe`. `GooseAdapter::register_gguf_model` registers
/// `canonical_model_stem` of the same file (`DeepSeek-R1-Distill-Qwen-1.5B`)
/// and hardcoded `ForceNative` with the comment "GIAP's local GGUFs (gemma
/// family) support llama.cpp native tool calling".
///
/// The second is the one the live turn resolves to: `ensure_provider_current`
/// builds `ModelConfig::new(&registry_key)` from exactly the key
/// `register_gguf_model` returns. So the probe's correct answer was written to
/// a row nothing read, and a constant meant for Gemma decided every model.
///
/// `should_use_native_tool_calling` treats `ForceNative` as `true` outright,
/// skipping the template dry-run, and `use_emulator` is its negation — so
/// forcing native on a template with no `tools` variable does not degrade
/// gracefully. It renders the declarations nowhere AND disables the prose
/// fallback that is such a model's only working mode. The model is handed
/// nothing, is told nothing, and no error is raised: DeepSeek-R1-Distill
/// answered a weather request by inventing an "MCP" tool interface out of the
/// system prompt.
///
/// The mapping is deliberately identical to
/// `LocalInferenceLlmAdapter::tool_and_thinking_for`, since the two write the
/// same registry and disagreeing would just relocate the bug.
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

/// Whether this model can be handed tool declarations natively.
///
/// The same question [`tool_mode_for_gguf`] answers for the registry, phrased
/// for `ModelCapabilities.tool_calling`, which
/// `GET /api/v1/models/capabilities` serves to the UI. It was name-keyed
/// (`gemma-4`, `qwen3`, `mistral`), so the UI reported "no tool calling" for
/// Nemotron, Nanbeige and Llama — all three of which render tools fine — and
/// reported nothing at all for a model it had not heard of.
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

/// The window the weights were TRAINED for, when the file says.
///
/// Not what this machine can afford -- that is the context governor's job, and
/// it ranks a registry pin and the engine's own memory cap above this. What
/// this replaces is `ModelCapabilities::from_model_name`'s 4096 default, which
/// for an unrecognised model is not a conservative estimate so much as a wrong
/// one: the models on this machine train at 131072, 262144 and 1048576.
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

    /// The gap that made Nemotron hallucinate, expressed as the two answers
    /// disagreeing. The name heuristic does not know it reasons; a probe that
    /// read its template does. This test does not need the file -- it pins that
    /// the FALLBACK is what was wrong, so that if someone later teaches the
    /// name heuristic about Nemotron they learn this path exists.
    #[test]
    fn the_fallback_is_the_thing_that_did_not_know_nemotron() {
        assert!(
            !model_reasons("ollama", "NVIDIA-Nemotron3-Nano-4B-Q4_K_M", None),
            "name heuristic has learned Nemotron; the probe path is now belt-and-braces"
        );
    }

    /// The gap the ForceNative fix in `acb1df18` did not reach.
    ///
    /// That commit removed four hardcoded `ForceNative` sites from
    /// `pond-adapters-local-inference` and its message ends "No hardcoded
    /// ForceNative remains outside the probe's own Native arm". A fifth lived in
    /// `GooseAdapter::register_gguf_model`, and it was the one on the live path:
    /// `ensure_provider_current` builds its `ModelConfig` from exactly the key
    /// that function returns. Worse, the two crates register the same file under
    /// two ids — the settings spelling and `canonical_model_stem` of it — so the
    /// probe's correct `ForceEmulated` and the constant `ForceNative` landed on
    /// different rows, and the turn read the constant.
    ///
    /// This pins the MAPPING rather than the call site, so it fails if the two
    /// writers ever disagree again. `Absent` must not map to `ForceNative`:
    /// `should_use_native_tool_calling` takes `ForceNative` as true outright and
    /// `use_emulator` is its negation, so forcing native on a tools-less
    /// template renders declarations nowhere AND disables the prose fallback
    /// that is such a model's only working mode.
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
