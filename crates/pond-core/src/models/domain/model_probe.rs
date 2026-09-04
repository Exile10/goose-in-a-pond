//! What a model can be asked to do, read from the model's own Jinja chat template rather than
//! from a name table: DeepSeek-R1-Distill has no `tools` variable, so forcing native tool calling
//! on it puts the declarations nowhere, and the MTP drafts carry no template at all — a third
//! state, not an error. Only whole words inside `{% ... %}` count; emitted text is not evidence.

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
    /// A header with no `chat_template` yields `Unknown`, not `Absent`: only "this model
    /// cannot" justifies withholding tools.
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
    /// `Unknown` answers no: a file that would not say is not evidence it would have said yes,
    /// and forcing native tools on a template with no `tools` variable is the failure here.
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

/// Read a probe from a GGUF on disk, remembering the answer. It is asked every turn from a
/// synchronous block and feeds `PromptState`, so it must be cheap (~40 ms a parse) and identical
/// on every turn: an answer that changes mid-session moves `prefix_hash` and costs a full
/// re-prefill, 3.7 s on the Orin. Keyed on `(path, mtime, len)`; a `None` is cached too.
pub fn probe_cached(path: &std::path::Path) -> Option<ModelProbe> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    /// `(path, mtime-as-nanos, len)`. `mtime` is `None` when the filesystem
    /// would not say, which simply makes the key coarser -- never wrong, since
    /// `len` still moves when the file does.
    type Key = (std::path::PathBuf, Option<u128>, u64);

    static CACHE: OnceLock<Mutex<HashMap<Key, Option<ModelProbe>>>> = OnceLock::new();

    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos());
    let key: Key = (path.to_path_buf(), mtime, meta.len());

    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(map) = cache.lock() {
        if let Some(hit) = map.get(&key) {
            return hit.clone();
        }
    }

    let probe = super::gguf::parse_gguf_file(path).map(|info| ModelProbe::from_gguf(&info));
    if let Ok(mut map) = cache.lock() {
        map.insert(key, probe.clone());
    }
    probe
}

/// Does `ident` appear as a whole word inside a Jinja control block?
///
/// The scan covers `{% ... %}` only: an identifier in emitted text says nothing about what the
/// template consumes, which is how DeepSeek-R1's one `tool_call` mention fools a substring test.
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

    /// The probe against every real GGUF on the machine, not excerpts. Excerpt fixtures prove
    /// the rules; only the files prove they survive 19 KB of real Jinja. Point
    /// `GIAP_TEST_GGUF_DIR` at the gguf models directory and run with `--ignored`.
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
