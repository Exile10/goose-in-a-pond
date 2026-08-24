use serde::{Deserialize, Serialize};

/// Runtime capabilities declared by an LLM provider.
///
/// Each adapter populates this based on the active model's known features.
/// Services and routes branch on these to enable model-specific behaviour
/// (thinking mode, vision input, larger context windows) while keeping the
/// core architecture model-agnostic.
///
/// All fields default to the most conservative assumption (false / 4096)
/// so that unknown models work safely out of the box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Model supports internal chain-of-thought reasoning.
    /// Gemma 4: `<|channel>thought...<channel|>`
    /// Qwen3 / DeepSeek-R1: `<think>...</think>`
    pub thinking: bool,

    /// Model accepts image content in messages (multimodal vision).
    pub vision: bool,

    /// Model accepts raw audio input (skip Whisper ASR).
    pub audio_input: bool,

    /// Maximum context window in tokens.
    pub context_window_tokens: u32,

    /// Supports constrained/structured output (GBNF grammar, JSON mode).
    pub structured_output: bool,

    /// Model supports native tool calling (e.g. Gemma 4 `<|tool_call>` format).
    /// When true, tool definitions are passed via the chat template.
    /// When false, tools are described in the system prompt text.
    pub tool_calling: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            thinking: false,
            vision: false,
            audio_input: false,
            context_window_tokens: 4096,
            structured_output: false,
            tool_calling: false,
        }
    }
}

/// Substrings that identify a multimodal (image-input) model family outright.
///
/// Each entry is distinctive enough that a plain `contains` cannot collide with
/// an unrelated model name. `llava` also covers `bakllava`.
const VISION_NAME_FRAGMENTS: &[&str] = &[
    "llava",
    "moondream",
    "pixtral",
    "minicpm-v",
    "minicpm_v",
    "minicpm-o",
    "internvl",
    "cogvlm",
    "smolvlm",
    "idefics",
    "multimodal",
    // Ollama publishes this one unseparated, so the `vl` SEGMENT rule below
    // cannot see it: `qwen2.5vl` splits into `qwen2` / `5vl`, never a bare `vl`.
    // The hyphenated `qwen2.5-vl` and `qwen3-vl` spellings are already covered
    // by that rule.
    "qwen2.5vl",
];

/// Whole name segments (split on any non-alphanumeric character) that identify
/// a multimodal model.
///
/// Segment matching rather than `contains`: `vl` as a bare substring appears
/// inside plenty of unrelated words, and a false positive here is the expensive
/// direction (see [`ModelCapabilities::name_implies_vision`]).
const VISION_NAME_SEGMENTS: &[&str] = &["vision", "vl", "vlm"];

/// Spellings of the Gemma 4 family, including the `gemma3n` name Ollama and
/// Hugging Face use for the same weights (`gemma3n:e4b`).
///
/// Consulted by the vision rule only. The other axes in
/// [`ModelCapabilities::from_model_name`] still match `gemma-4*` literally:
/// widening them would silently change thinking, tool-calling and context-window
/// behaviour — and with it the prompt tier — for models beyond the vision bug
/// this list was added for.
const GEMMA4_NAME_FRAGMENTS: &[&str] = &[
    "gemma-4", "gemma4", "gemma_4", "gemma-3n", "gemma3n", "gemma_3n",
];

impl ModelCapabilities {
    /// Whether a model NAME is evidence that the model accepts image input.
    ///
    /// # Why this is deliberately asymmetric
    ///
    /// The two error directions do not cost the same. A false POSITIVE reaches
    /// the system prompt (`<vision>`: "you can see images"), so a text-only
    /// model is told it has an ability it does not have and answers an
    /// image question by inventing an image. A false NEGATIVE only withholds
    /// that section — the model stays as it was, which is the status quo the
    /// section exists to improve. So the bias is heavily towards `false`:
    /// anything unrecognised stays `false`, and there is one explicit exclusion
    /// (below) for a family member that breaks its family's rule.
    ///
    /// # What the rules are NOT
    ///
    /// Two of the three rules name a known family outright
    /// (`VISION_NAME_FRAGMENTS`, `GEMMA4_NAME_FRAGMENTS`). The third —
    /// `VISION_NAME_SEGMENTS` — is a *marker* rule, not a family
    /// identification: it credits any name carrying `vision` / `vl` / `vlm` as a
    /// whole segment. That is what makes it cover the long tail of vendors and
    /// quantisers who label a multimodal build that way without appearing in any
    /// list here, and it is why `qwen2.5-vl`, `qwen3-vl` and `llama3.2-vision`
    /// need no entry of their own. The cost is that a name which merely contains
    /// the segment for an unrelated reason — `vision-labs/text-only-7b` — is
    /// credited too. Segment matching (rather than `contains`) keeps that to
    /// contrived names; it is accepted, not solved.
    ///
    /// # The exclusion
    ///
    /// Every featured Gemma 4 declares a vision encoder EXCEPT `E1B`
    /// (`FEATURED_MODELS` in `goose-local-inference`, where `E1B` is the one
    /// entry with `mmproj: None`). On the in-process engine that registry IS
    /// the answer and this function is not consulted; on an HTTP provider
    /// serving the same weights the name is all there is, so the exclusion has
    /// to be repeated here or `gemma-4-E1B-it` on Ollama is told it can see.
    ///
    /// # Scope
    ///
    /// Ollama/llamafile-class open models only. Cloud models are not listed:
    /// they are vision-capable, but they also do not exhibit the failure this
    /// signal drives (a 4B model insisting it is "a text-based assistant"),
    /// and every added entry is a claim this file has to keep true.
    #[must_use]
    pub fn name_implies_vision(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();

        if GEMMA4_NAME_FRAGMENTS.iter().any(|f| lower.contains(f)) {
            return !lower.contains("e1b");
        }

        VISION_NAME_FRAGMENTS.iter().any(|f| lower.contains(f))
            || lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|segment| VISION_NAME_SEGMENTS.contains(&segment))
    }

    /// Detect capabilities from a model name string.
    ///
    /// This is a heuristic based on known model families. Adapters can
    /// override with more precise information from provider APIs.
    pub fn from_model_name(name: &str) -> Self {
        let lower = name.to_lowercase();
        let mut caps = Self::default();

        // Thinking-capable model families
        if lower.contains("gemma-4")
            || lower.contains("gemma4")
            || lower.contains("gemma_4")
            || lower.contains("qwen3")
            || lower.contains("qwq")
            || lower.contains("deepseek-r1")
            || lower.contains("deepseek_r1")
        {
            caps.thinking = true;
        }

        // Vision-capable model families. Kept in one place (and deliberately
        // narrower than the other axes below) because this flag is the only one
        // that can put a claim about the model's own senses into its prompt.
        caps.vision = Self::name_implies_vision(name);

        // Audio-capable (Gemma 4 E2B/E4B only)
        if (lower.contains("gemma-4") || lower.contains("gemma4") || lower.contains("gemma_4"))
            && (lower.contains("e2b") || lower.contains("e4b"))
        {
            caps.audio_input = true;
        }

        // Context window heuristics
        if lower.contains("gemma-4") || lower.contains("gemma4") || lower.contains("gemma_4") {
            // Gemma 4 E2B/E4B: 128K, 26B/31B: 256K
            if lower.contains("e2b") || lower.contains("e4b") {
                caps.context_window_tokens = 128_000;
            } else {
                caps.context_window_tokens = 128_000; // conservative for GGUF
            }
        } else if lower.contains("llama-3") || lower.contains("llama3") {
            caps.context_window_tokens = 8_192;
        } else if lower.contains("qwen") {
            caps.context_window_tokens = 32_768;
        } else if lower.contains("mistral") {
            caps.context_window_tokens = 32_768;
        }

        // Structured output — all local GGUF models support GBNF via llama.cpp
        if lower.contains(".gguf")
            || lower.contains("q4_k")
            || lower.contains("q5_k")
            || lower.contains("q8_0")
            || lower.contains("q6_k")
        {
            caps.structured_output = true;
        }

        // Native tool calling — Gemma 4 uses <|tool_call> format via Jinja template
        if lower.contains("gemma-4")
            || lower.contains("gemma4")
            || lower.contains("gemma_4")
            || lower.contains("qwen3")
            || lower.contains("mistral")
        {
            caps.tool_calling = true;
        }

        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_conservative() {
        let caps = ModelCapabilities::default();
        assert!(!caps.thinking);
        assert!(!caps.vision);
        assert!(!caps.audio_input);
        assert_eq!(caps.context_window_tokens, 4096);
        assert!(!caps.structured_output);
        assert!(!caps.tool_calling);
    }

    #[test]
    fn detects_gemma4_capabilities() {
        let caps = ModelCapabilities::from_model_name("gemma-4-E2B-it-Q4_K_M.gguf");
        assert!(caps.thinking);
        assert!(caps.vision);
        assert!(caps.audio_input);
        assert_eq!(caps.context_window_tokens, 128_000);
        assert!(caps.structured_output);
    }

    #[test]
    fn detects_qwen3_thinking() {
        let caps = ModelCapabilities::from_model_name("qwen3-8b-instruct-q4_k_m");
        assert!(caps.thinking);
        assert!(!caps.vision);
        assert_eq!(caps.context_window_tokens, 32_768);
    }

    #[test]
    fn llama_defaults() {
        let caps = ModelCapabilities::from_model_name("llama3.2");
        assert!(!caps.thinking);
        assert!(!caps.vision);
        assert_eq!(caps.context_window_tokens, 8_192);
    }

    #[test]
    fn unknown_model_gets_safe_defaults() {
        let caps = ModelCapabilities::from_model_name("my-custom-model");
        assert!(!caps.thinking);
        assert!(!caps.vision);
        assert_eq!(caps.context_window_tokens, 4096);
    }

    // ── Vision detection ──────────────────────────────────────────────────

    /// The headline false positive. E1B is the one Gemma 4 with no vision
    /// encoder — it is literally the entry the local registry excludes via
    /// `mmproj: None` — so a name rule that says "gemma-4 means vision" tells a
    /// blind model it can see.
    #[test]
    fn gemma4_e1b_has_no_vision_in_any_spelling() {
        for name in [
            "gemma-4-E1B-it",
            "gemma-4-E1B-it-Q4_K_M.gguf",
            "gemma4-e1b",
            "gemma3n:e1b",
        ] {
            assert!(
                !ModelCapabilities::from_model_name(name).vision,
                "{name} declares no mmproj encoder"
            );
        }
    }

    #[test]
    fn the_gemma4_variants_that_do_carry_an_encoder_are_recognised() {
        for name in [
            "gemma-4-E2B-it",
            "gemma-4-E4B-it-Q4_K_M",
            "gemma-4-12B-A4B-it",
            "gemma-4-27B-it",
            // The spelling Ollama serves the same weights under.
            "gemma3n:e4b",
            "gemma3n:e2b",
            "gemma-3n-E4B-it",
        ] {
            assert!(
                ModelCapabilities::from_model_name(name).vision,
                "{name} is multimodal"
            );
        }
    }

    /// The false negatives that left the production bug ("I cannot describe
    /// images, I am a text-based assistant") unfixed on every HTTP provider.
    #[test]
    fn common_http_vision_models_are_recognised() {
        for name in [
            "llama3.2-vision",
            "llama3.2-vision:11b",
            "llama-3.2-90b-vision-instruct",
            "qwen2.5-vl",
            "qwen2.5-vl:7b",
            // Ollama's real published tag has no hyphen, so the `vl` segment
            // rule cannot see it and it needs its own fragment.
            "qwen2.5vl",
            "qwen2.5vl:7b",
            "Qwen2-VL-7B-Instruct",
            "qwen3-vl:8b",
            "minicpm-v",
            "minicpm-v:8b",
            "pixtral-12b",
            "llava:13b",
            "bakllava",
            "moondream",
            "internvl2-8b",
            "phi-4-multimodal-instruct",
        ] {
            assert!(
                ModelCapabilities::from_model_name(name).vision,
                "{name} accepts images"
            );
        }
    }

    /// Silence is the safe answer, so text-only models must stay false — and a
    /// bare `vl`/`vision` SUBSTRING must not be what decides it.
    #[test]
    fn text_only_models_are_not_credited_with_vision() {
        for name in [
            "llama3.2",
            "llama3.2:3b",
            "Llama-3.2-3B-Instruct",
            "qwen3-8b-instruct-q4_k_m",
            "mistral-small-24b",
            "Hermes-2-Pro-Mistral-7B",
            "gpt-oss:20b",
            "my-custom-model",
            // "vl" inside a word is not a vision marker.
            "vlad-tuned-7b",
            "nvlink-test-model",
        ] {
            assert!(
                !ModelCapabilities::from_model_name(name).vision,
                "{name} has no image input"
            );
        }
    }

    #[test]
    fn serialization_roundtrip() {
        let caps = ModelCapabilities::from_model_name("gemma-4-E2B-it-Q4_K_M.gguf");
        let json = serde_json::to_string(&caps).unwrap();
        let caps2: ModelCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(caps.thinking, caps2.thinking);
        assert_eq!(caps.vision, caps2.vision);
        assert_eq!(caps.context_window_tokens, caps2.context_window_tokens);
    }
}

#[cfg(test)]
mod probe_gap_tests {
    use super::*;

    /// The gap that made Nemotron hallucinate instead of calling a tool.
    ///
    /// `thinking_mode = "auto"` resolves through this function, which reads the
    /// FILENAME. Nemotron reasons -- its template gates a `<think>` block and
    /// `ModelProbe` reads that correctly -- but nothing in the name says so, so
    /// the prompt-side thinking section was omitted while the engine had
    /// `enable_thinking = true`. Measured on 2026-08-24: with `auto` the model
    /// produced an empty first turn, got re-engaged, and fabricated a weather
    /// report; with `"on"` it called the weather tool and reported real data.
    #[test]
    fn the_name_heuristic_does_not_know_nemotron_reasons() {
        let caps = ModelCapabilities::from_model_name("NVIDIA-Nemotron3-Nano-4B-Q4_K_M");
        assert!(
            !caps.thinking,
            "if the name heuristic has learned Nemotron, this gap is closed and \
             thinking_mode=auto no longer needs the probe"
        );
    }
}
