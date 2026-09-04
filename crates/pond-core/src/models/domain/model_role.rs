//! Which job a model has been given: the single source for the seven role strings that five
//! sites used to match on separately. [`ModelRole::settings_mirror`] is load-bearing, and an
//! empty answer is a real answer; a Piper voice must never be mirrored, since `tts_piper/<name>`
//! is an id no catalogue row has and every guard against the catalogue then stops matching.

use crate::models::domain::model_record::ModelCategory;

/// Categories that can serve a language-model role.
const LLM_CATEGORIES: [ModelCategory; 3] = [
    ModelCategory::Gguf,
    ModelCategory::Llamafile,
    ModelCategory::Ollama,
];
const ASR_CATEGORIES: [ModelCategory; 1] = [ModelCategory::Whisper];
/// Kokoro first: it is the only live engine, so a forgiving lookup that probes
/// in this order finds what a household means before it finds a legacy row.
const TTS_CATEGORIES: [ModelCategory; 3] = [
    ModelCategory::TtsKokoro,
    ModelCategory::TtsHttp,
    ModelCategory::TtsPiper,
];
const EMBEDDING_CATEGORIES: [ModelCategory; 1] = [ModelCategory::Embedding];

/// A job a model can be assigned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelRole {
    /// Fast conversational model for everyday queries.
    Chat,
    /// Deeper reasoning model. Assignable and validated, but selects nothing
    /// today — see the module note.
    Think,
    /// Agentic tool-use model. Same standing as [`Self::Think`].
    Task,
    /// The model consulted for tool selection.
    Tool,
    /// Speech to text.
    Asr,
    /// Text to speech.
    Tts,
    /// Vector embeddings for the personal-context index.
    Embedding,
}

impl ModelRole {
    /// Every role, in the order surfaces should present them.
    pub const ALL: [ModelRole; 7] = [
        Self::Chat,
        Self::Think,
        Self::Task,
        Self::Tool,
        Self::Asr,
        Self::Tts,
        Self::Embedding,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Think => "think",
            Self::Task => "task",
            Self::Tool => "tool",
            Self::Asr => "asr",
            Self::Tts => "tts",
            Self::Embedding => "embedding",
        }
    }

    /// Parse a role name. `None` for anything else — roles arrive from the API
    /// and the CLI as free text, and an unknown one is a refusal, not a panic.
    pub fn from_str(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|r| r.as_str() == s)
    }

    /// The categories a model may belong to to take this role, best first.
    pub fn candidate_categories(&self) -> &'static [ModelCategory] {
        match self {
            Self::Chat | Self::Think | Self::Task | Self::Tool => &LLM_CATEGORIES,
            Self::Asr => &ASR_CATEGORIES,
            Self::Tts => &TTS_CATEGORIES,
            Self::Embedding => &EMBEDDING_CATEGORIES,
        }
    }

    /// Whether a model of `category` may take this role.
    pub fn accepts(&self, category: &ModelCategory) -> bool {
        self.candidate_categories().contains(category)
    }

    /// True for roles whose model is a language model.
    pub fn is_llm(&self) -> bool {
        matches!(self, Self::Chat | Self::Think | Self::Task | Self::Tool)
    }

    /// True only for the role whose model the live provider actually serves.
    ///
    /// Distinct from [`Self::is_llm`]: rebuilding the provider for `think` or `task` would
    /// discard the KV prompt cache and force a full prefill for a setting nothing reads.
    pub fn rebuilds_llm_provider(&self) -> bool {
        matches!(self, Self::Chat)
    }

    /// Every settings key this role owns. A patch touching one of these is a
    /// patch that must re-sync this role's assignment.
    pub fn settings_keys(&self) -> &'static [&'static str] {
        match self {
            Self::Chat => &["chat_provider", "chat_model"],
            Self::Think | Self::Task => &[],
            Self::Tool => &["tool_model"],
            Self::Asr => &["active_whisper_model"],
            // `voice_tts_voice` belongs to this role too: the Voice screen
            // writes it without touching the assignment, so a sweep that
            // ignored it left the picker and the assignment disagreeing.
            Self::Tts => &["active_tts_model", "voice_tts_voice"],
            Self::Embedding => &["active_embedding_model"],
        }
    }

    /// The role that owns `key`, if any.
    pub fn for_settings_key(key: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|r| r.settings_keys().contains(&key))
    }

    /// The ONE role-to-settings mapping: which keys a writer upserts when `model_name` of
    /// `category` takes this role. Empty is legitimate: `Think`/`Task` select no model, and a
    /// legacy Piper voice is a filename, not a catalogue name. Kokoro writes both keys, or the
    /// spoken voice lags the picker. `embedding_provider` is absent: writing it selects fastembed.
    pub fn settings_mirror(
        &self,
        category: &ModelCategory,
        model_name: &str,
    ) -> Vec<(&'static str, String)> {
        match self {
            Self::Chat => vec![
                ("chat_provider", category.runtime_provider().to_string()),
                ("chat_model", model_name.to_string()),
            ],
            Self::Think | Self::Task => Vec::new(),
            Self::Tool => vec![("tool_model", model_name.to_string())],
            Self::Asr => vec![("active_whisper_model", model_name.to_string())],
            Self::Tts => match category {
                ModelCategory::TtsPiper => Vec::new(),
                ModelCategory::TtsKokoro => vec![
                    ("active_tts_model", model_name.to_string()),
                    ("voice_tts_voice", model_name.to_string()),
                ],
                _ => vec![("active_tts_model", model_name.to_string())],
            },
            Self::Embedding => vec![("active_embedding_model", model_name.to_string())],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_round_trips_through_its_name() {
        for role in ModelRole::ALL {
            assert_eq!(ModelRole::from_str(role.as_str()), Some(role));
        }
        assert_eq!(ModelRole::from_str("mixer"), None);
        assert_eq!(ModelRole::from_str(""), None);
    }

    /// The vocabulary this enum was widened to own. Five sites each held their
    /// own subset; a role missing here is a role that becomes unassignable or
    /// unreportable somewhere.
    #[test]
    fn the_role_vocabulary_is_the_seven_the_pond_runs_on() {
        let names: Vec<&str> = ModelRole::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            names,
            ["chat", "think", "task", "tool", "asr", "tts", "embedding"]
        );
    }

    #[test]
    fn a_role_accepts_exactly_the_categories_that_can_serve_it() {
        assert!(ModelRole::Chat.accepts(&ModelCategory::Gguf));
        assert!(ModelRole::Tool.accepts(&ModelCategory::Ollama));
        assert!(!ModelRole::Chat.accepts(&ModelCategory::Whisper));
        assert!(ModelRole::Asr.accepts(&ModelCategory::Whisper));
        assert!(!ModelRole::Asr.accepts(&ModelCategory::Gguf));
        assert!(ModelRole::Tts.accepts(&ModelCategory::TtsKokoro));
        assert!(ModelRole::Tts.accepts(&ModelCategory::TtsPiper));
        assert!(ModelRole::Embedding.accepts(&ModelCategory::Embedding));
        assert!(!ModelRole::Embedding.accepts(&ModelCategory::Gguf));
    }

    /// The defect the mirror exists for: the reverse sweep hardcoded the TTS
    /// category, so a Kokoro voice was written as `tts_piper/<name>` — an id
    /// no catalogue row has, which silently unticked the live voice.
    #[test]
    fn a_kokoro_voice_writes_both_the_catalogue_row_and_the_engine_voice() {
        let mirror = ModelRole::Tts.settings_mirror(&ModelCategory::TtsKokoro, "af_heart");
        assert_eq!(
            mirror,
            vec![
                ("active_tts_model", "af_heart".to_string()),
                ("voice_tts_voice", "af_heart".to_string()),
            ]
        );
    }

    /// Piper is read-only legacy: it must parse, and must never be mirrored.
    #[test]
    fn a_legacy_piper_voice_is_never_written_to_settings() {
        assert!(ModelRole::Tts
            .settings_mirror(&ModelCategory::TtsPiper, "en_US-ryan-high.onnx")
            .is_empty());
    }

    /// Empty is a real answer, not a gap: these roles select no model today.
    #[test]
    fn think_and_task_are_assignable_but_mirror_nothing() {
        for role in [ModelRole::Think, ModelRole::Task] {
            assert!(role.accepts(&ModelCategory::Gguf), "must stay assignable");
            assert!(role.settings_mirror(&ModelCategory::Gguf, "m").is_empty());
            assert!(role.settings_keys().is_empty());
        }
    }

    #[test]
    fn chat_mirrors_the_runtime_provider_not_the_category_name() {
        assert_eq!(
            ModelRole::Chat.settings_mirror(&ModelCategory::Gguf, "gemma"),
            vec![
                ("chat_provider", "local".to_string()),
                ("chat_model", "gemma".to_string()),
            ]
        );
        assert_eq!(
            ModelRole::Chat.settings_mirror(&ModelCategory::Ollama, "llama3.2")[0].1,
            "ollama"
        );
    }

    /// Only chat's model is what the live provider serves — the others would
    /// force a pointless unload/reload that discards the KV prompt cache.
    #[test]
    fn only_chat_rebuilds_the_provider_though_four_roles_are_llm() {
        let llm: Vec<&str> = ModelRole::ALL
            .iter()
            .filter(|r| r.is_llm())
            .map(|r| r.as_str())
            .collect();
        assert_eq!(llm, ["chat", "think", "task", "tool"]);
        let rebuilds: Vec<&str> = ModelRole::ALL
            .iter()
            .filter(|r| r.rebuilds_llm_provider())
            .map(|r| r.as_str())
            .collect();
        assert_eq!(rebuilds, ["chat"]);
    }

    /// A settings key belongs to at most one role, or a reverse sweep would
    /// have to guess which assignment a save meant.
    #[test]
    fn no_settings_key_is_claimed_by_two_roles() {
        let mut seen: Vec<&str> = Vec::new();
        for role in ModelRole::ALL {
            for key in role.settings_keys() {
                assert!(!seen.contains(key), "{key} is claimed twice");
                seen.push(key);
                assert_eq!(ModelRole::for_settings_key(key), Some(role));
            }
        }
        assert_eq!(ModelRole::for_settings_key("user_name"), None);
    }

    /// Every key a role mirrors must be a key that role owns, or a save of it
    /// would never re-sync the assignment that wrote it.
    #[test]
    fn every_mirrored_key_is_owned_by_the_role_that_writes_it() {
        for role in ModelRole::ALL {
            for category in role.candidate_categories() {
                for (key, _) in role.settings_mirror(category, "m") {
                    assert!(
                        role.settings_keys().contains(&key),
                        "{} mirrors {key} but does not own it",
                        role.as_str()
                    );
                }
            }
        }
    }
}
