//! What a model can be asked to do, read from the model.
//!
//! Three mechanisms decide this today and none of them looks at the model: GIAP
//! forces `ToolCallingMode::ForceNative` on every model, goose matches a
//! hardcoded table of featured repos, and `ModelCapabilities::from_model_name`
//! substring-matches the filename. A model none of them has heard of gets
//! native tool calling forced onto it whether or not its template can render
//! tools, and thinking enabled whether or not it has any.
//!
//! The answer is in the file. A GGUF carries the model's own Jinja chat
//! template, and that template is the thing the tokens are actually rendered
//! through — so whether it accepts a `tools` variable, and whether it gates
//! reasoning, are facts rather than inferences.
//!
//! # What the evidence looked like
//!
//! Read off the eleven GGUFs on the development machine, five architectures:
//!
//! | model | arch | `tools` in control flow | gate | marker |
//! |---|---|---|---|---|
//! | Gemma 4 E2B/E4B/12B | gemma4 | yes | `enable_thinking` | `<\|think\|>` |
//! | Nemotron3-Nano-4B | nemotron_h | yes | `enable_thinking` | `<think>` |
//! | Nanbeige4.2-3B | nanbeige | yes | `enable_thinking` | `<think>` |
//! | DeepSeek-R1-Distill-Qwen | qwen2 | **no** | none | `<think>` |
//! | gemma-4-*-assistant (MTP) | gemma4_mtp | no template at all | — | — |
//!
//! Two of those rows are the reason this exists. **DeepSeek-R1-Distill has no
//! `tools` variable**, so forcing native tool calling on it puts declarations
//! nowhere. And the MTP drafts carry no template at all, which is a third state
//! rather than an error.
//!
//! # Why counting substrings is not enough
//!
//! DeepSeek's template mentions `tool_call` once while supporting no tools, so
//! a `contains("tool")` test calls it a tool user. The distinction that holds is
//! between template *control flow* — `{%- if tools -%}`, `{%- for tool in
//! tools %}` — and an identifier appearing in emitted text. This module looks
//! only inside `{% ... %}` blocks, and only for the whole word.

use super::gguf::GgufInfo;

/// Whether tool declarations can be rendered at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSupport {
    /// The template takes a `tools` variable and renders it. Declarations can
    /// be passed natively.
    Native,
    /// No `tools` variable. Tools must be described in prose in the system
    /// prompt, or not offered.
    Absent,
    /// No template to read. Says nothing either way.
    Unknown,
}

/// Whether the model reasons before answering, and how that is controlled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thinking {
    /// The template exposes a flag (`enable_thinking`) and the caller decides.
    /// Gemma 4, Nemotron, Nanbeige.
    Gated { marker: String },
    /// The template always opens a reasoning block; there is no flag to clear.
    /// DeepSeek-R1 distills.
    Always { marker: String },
    /// No reasoning markers.
    Absent,
    /// No template to read.
    Unknown,
}

/// What a model's own file says it can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProbe {
    pub tools: ToolSupport,
    pub thinking: Thinking,
    /// `{arch}.context_length` — what the weights were trained for, which is
    /// not the same as what this machine can afford.
    pub context_window_tokens: Option<u32>,
    /// `general.architecture`, for rules that legitimately need the family.
    pub architecture: Option<String>,
}

/// Reasoning markers, most specific first.
///
/// Gemma 4 emits `<|think|>`, NOT the `<|channel>thought` that
/// `ModelCapabilities` documents -- read from the template on 2026-08-16.
const THINKING_MARKERS: &[&str] = &["<|think|>", "<think>", "<|channel|>", "<reasoning>"];

impl ModelProbe {
    /// Read a probe from a parsed GGUF header.
    ///
    /// A header with no `chat_template` yields `Unknown` rather than `Absent`:
    /// "this file did not say" and "this model cannot" are different, and only
    /// the second justifies withholding tools.
    pub fn from_gguf(info: &GgufInfo) -> Self {
        let Some(template) = info.chat_template.as_deref() else {
            return Self {
                tools: ToolSupport::Unknown,
                thinking: Thinking::Unknown,
                context_window_tokens: info.context_length,
                architecture: info.architecture.clone(),
            };
        };

        let tools = if mentions_in_control_flow(template, "tools") {
            ToolSupport::Native
        } else {
            ToolSupport::Absent
        };

        let marker = THINKING_MARKERS
            .iter()
            .find(|m| template.contains(**m))
            .map(|m| (*m).to_string());

        let thinking = match marker {
            Some(marker) if mentions_in_control_flow(template, "enable_thinking") => {
                Thinking::Gated { marker }
            }
            Some(marker) => Thinking::Always { marker },
            None => Thinking::Absent,
        };

        Self {
            tools,
            thinking,
            context_window_tokens: info.context_length,
            architecture: info.architecture.clone(),
        }
    }

    /// Can tool declarations be handed to this model natively?
    ///
    /// `Unknown` answers no. Forcing native tool calling on a model whose
    /// template turns out not to take a `tools` variable is the failure this
    /// module exists to prevent, and a file that would not say is not evidence
    /// that it would have said yes.
    pub fn supports_native_tools(&self) -> bool {
        matches!(self.tools, ToolSupport::Native)
    }

    /// Should reasoning be switched on for this model?
    ///
    /// Only where the template offers the flag. `Always` needs no help and
    /// `Absent` has nothing to switch.
    pub fn thinking_is_selectable(&self) -> bool {
        matches!(self.thinking, Thinking::Gated { .. })
    }

    /// The tag this model opens reasoning with, when it has one.
    pub fn thinking_marker(&self) -> Option<&str> {
        match &self.thinking {
            Thinking::Gated { marker } | Thinking::Always { marker } => Some(marker),
            _ => None,
        }
    }
}

/// Does `ident` appear as a whole word inside a Jinja control block?
///
/// The scan is over `{% ... %}` only. Text outside a control block is what the
/// template *emits*, and an identifier there says nothing about whether the
/// template consumes a variable of that name -- which is exactly how
/// DeepSeek-R1's single mention of `tool_call` reads as tool support to a
/// substring test.
fn mentions_in_control_flow(template: &str, ident: &str) -> bool {
    let mut rest = template;
    while let Some(open) = rest.find("{%") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("%}") else {
            return false;
        };
        if contains_word(&after[..close], ident) {
            return true;
        }
        rest = &after[close + 2..];
    }
    false
}

/// Whole-word match, so `tools` does not match `tool_calls` and `tool` does not
/// match `tools`.
fn contains_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(word) {
        let start = from + rel;
        let end = start + word.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
        if from >= haystack.len() {
            break;
        }
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Excerpts taken verbatim from the real templates on 2026-08-16, so the
    /// rules are tested against the strings they will actually meet.
    const GEMMA4: &str = r#"{{- bos_token -}}
        {%- if (enable_thinking is defined and enable_thinking) or tools or messages[0]['role'] in ['system'] -%}
        {{- '<|turn>system\n' -}}
        {%- if enable_thinking is defined and enable_thinking -%}{{- '<|think|>\n' -}}{%- endif -%}
        {%- if tools -%}{%- for tool in tools %}{{- '<|tool>' -}}{%- endfor -%}{%- endif -%}"#;

    const DEEPSEEK_R1: &str = r#"{% if ns.is_tool %}{{'<|tool outputs end|>'}}{% endif %}
        {% if add_generation_prompt and not ns.is_tool %}{{'<|Assistant|><think>\n'}}{% endif %}"#;

    const NO_THINKING: &str = r#"{%- if tools %}{%- for tool in tools %}{{ tool }}{%- endfor %}{%- endif %}
        {%- for message in messages %}{{ message['content'] }}{%- endfor %}"#;

    fn info(template: Option<&str>) -> GgufInfo {
        GgufInfo {
            architecture: Some("test".into()),
            context_length: Some(131072),
            chat_template: template.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn gemma_is_a_tool_user_and_a_gated_thinker() {
        let p = ModelProbe::from_gguf(&info(Some(GEMMA4)));
        assert_eq!(p.tools, ToolSupport::Native);
        assert!(p.supports_native_tools());
        assert_eq!(
            p.thinking,
            Thinking::Gated {
                marker: "<|think|>".into()
            }
        );
        assert!(p.thinking_is_selectable());
        assert_eq!(p.context_window_tokens, Some(131072));
    }

    /// The row this module exists for. DeepSeek reasons but cannot be handed
    /// tools, and GIAP forces native tool calling on every model regardless.
    #[test]
    fn deepseek_reasons_but_is_not_a_tool_user() {
        let p = ModelProbe::from_gguf(&info(Some(DEEPSEEK_R1)));
        assert_eq!(
            p.tools,
            ToolSupport::Absent,
            "DeepSeek-R1 has no `tools` variable; forcing native tool calling puts \
             declarations nowhere"
        );
        assert!(!p.supports_native_tools());
        assert_eq!(
            p.thinking,
            Thinking::Always {
                marker: "<think>".into()
            },
            "its <think> block is opened unconditionally -- there is no flag to clear"
        );
        assert!(!p.thinking_is_selectable());
    }

    /// The false positive that a substring test produces, pinned.
    #[test]
    fn a_mention_of_tool_call_is_not_tool_support() {
        assert!(
            DEEPSEEK_R1.contains("tool"),
            "fixture must contain the word, or this test proves nothing"
        );
        assert!(!mentions_in_control_flow(DEEPSEEK_R1, "tools"));
    }

    #[test]
    fn a_tool_user_that_does_not_reason() {
        let p = ModelProbe::from_gguf(&info(Some(NO_THINKING)));
        assert_eq!(p.tools, ToolSupport::Native);
        assert_eq!(p.thinking, Thinking::Absent);
        assert_eq!(p.thinking_marker(), None);
    }

    /// No template is a third state. The MTP drafts on this machine carry
    /// none, and "did not say" must not be read as "cannot".
    #[test]
    fn no_template_is_unknown_not_absent() {
        let p = ModelProbe::from_gguf(&info(None));
        assert_eq!(p.tools, ToolSupport::Unknown);
        assert_eq!(p.thinking, Thinking::Unknown);
        assert!(
            !p.supports_native_tools(),
            "Unknown must not be treated as permission to force native tools"
        );
        assert_eq!(
            p.context_window_tokens,
            Some(131072),
            "metadata still answers even when the template is missing"
        );
    }

    /// The probe against every real GGUF on the machine, not excerpts.
    ///
    /// Excerpt fixtures prove the rules; only the files prove the rules survive
    /// contact with 19 KB of real Jinja. Run with
    ///
    /// ```text
    /// GIAP_TEST_GGUF_DIR="$HOME/Library/Application Support/goose-in-a-pond/models/gguf" \
    ///   cargo test -p pond-core --lib model_probe -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs real GGUF files; set GIAP_TEST_GGUF_DIR"]
    fn probes_every_model_on_disk() {
        use crate::models::domain::gguf::parse_gguf_file;

        let Ok(dir) = std::env::var("GIAP_TEST_GGUF_DIR") else {
            eprintln!("GIAP_TEST_GGUF_DIR unset");
            return;
        };
        let (mut tool_users, mut thinkers, mut seen) = (0, 0, 0);
        for entry in std::fs::read_dir(&dir).expect("dir") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                continue;
            }
            let Some(info) = parse_gguf_file(&path) else {
                continue;
            };
            let p = ModelProbe::from_gguf(&info);
            eprintln!(
                "{:<44} {:<8?} {:?}",
                path.file_name().unwrap().to_string_lossy(),
                p.tools,
                p.thinking
            );
            seen += 1;
            if p.supports_native_tools() {
                tool_users += 1;
            }
            if p.thinking_marker().is_some() {
                thinkers += 1;
            }

            // Whatever a template says, an absent one must never read as
            // permission to force native tools.
            if info.chat_template.is_none() {
                assert!(
                    !p.supports_native_tools(),
                    "no template must not grant tools"
                );
            }
        }
        assert!(seen > 0, "no GGUF files under {dir}");
        assert!(
            tool_users > 0 && tool_users < seen,
            "the probe should separate the collection, not answer the same for all {seen}: \
             {tool_users} tool users"
        );
        assert!(
            thinkers > 0,
            "no reasoning markers found across {seen} models"
        );
    }

    #[test]
    fn whole_words_only() {
        assert!(contains_word("{% if tools %}", "tools"));
        assert!(!contains_word("{% if tool_calls %}", "tools"));
        assert!(!contains_word("{% if tools %}", "tool"));
        assert!(contains_word("{%- for tool in tools %}", "tool"));
        assert!(!contains_word("{{ mytools }}", "tools"));
    }

    /// Only control blocks count; emitted text does not.
    #[test]
    fn emitted_text_is_not_control_flow() {
        assert!(!mentions_in_control_flow(
            "{{ 'tools are great' }}",
            "tools"
        ));
        assert!(mentions_in_control_flow("{% if tools %}", "tools"));
        // An unterminated block must not loop or panic.
        assert!(!mentions_in_control_flow("{% if tools", "tools"));
    }
}
