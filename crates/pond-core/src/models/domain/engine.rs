//! The engine that runs a conversation model, read from its catalogue category.

use serde::{Deserialize, Serialize};

use super::model_record::ModelCategory;

/// goose's registry `backend_id` for LiteRT-LM rows. goose keeps its own copy private.
pub const LITERT_BACKEND_ID: &str = "litert";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Engine {
    #[serde(rename = "llama_cpp")]
    LlamaCpp,
    #[serde(rename = "litert_lm")]
    LiteRtLm,
    #[serde(rename = "ollama")]
    Ollama,
    #[serde(rename = "llamafile")]
    Llamafile,
}

impl Engine {
    pub const ALL: [Engine; 4] = [
        Engine::LlamaCpp,
        Engine::LiteRtLm,
        Engine::Ollama,
        Engine::Llamafile,
    ];

    /// `None` for the categories no conversation engine runs: speech, voices and embeddings.
    pub fn for_category(category: &ModelCategory) -> Option<Self> {
        match category {
            ModelCategory::Gguf => Some(Self::LlamaCpp),
            ModelCategory::Litert => Some(Self::LiteRtLm),
            ModelCategory::Ollama => Some(Self::Ollama),
            ModelCategory::Llamafile => Some(Self::Llamafile),
            ModelCategory::Whisper
            | ModelCategory::TtsPiper
            | ModelCategory::TtsKokoro
            | ModelCategory::TtsHttp
            | ModelCategory::Embedding => None,
        }
    }

    pub fn category(self) -> ModelCategory {
        match self {
            Self::LlamaCpp => ModelCategory::Gguf,
            Self::LiteRtLm => ModelCategory::Litert,
            Self::Ollama => ModelCategory::Ollama,
            Self::Llamafile => ModelCategory::Llamafile,
        }
    }

    /// The serialised id.
    pub fn id(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama_cpp",
            Self::LiteRtLm => "litert_lm",
            Self::Ollama => "ollama",
            Self::Llamafile => "llamafile",
        }
    }

    /// The engine's own name, as the household reads it.
    pub fn label(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama.cpp",
            Self::LiteRtLm => "LiteRT-LM",
            Self::Ollama => "Ollama",
            Self::Llamafile => "llamafile",
        }
    }

    /// The file extension the engine loads; `None` for Ollama, which keeps its own store.
    pub fn file_format(self) -> Option<&'static str> {
        match self {
            Self::LlamaCpp => Some(".gguf"),
            Self::LiteRtLm => Some(".litertlm"),
            Self::Ollama => None,
            Self::Llamafile => Some(".llamafile"),
        }
    }

    /// Runs inside the pond's process, so its weights count against the pond's own memory.
    pub fn in_process(self) -> bool {
        matches!(self, Self::LlamaCpp | Self::LiteRtLm)
    }

    /// The goose registry's `backend_id` for the engine; `None` is goose's default, llama.cpp.
    pub fn backend_id(self) -> Option<&'static str> {
        match self {
            Self::LiteRtLm => Some(LITERT_BACKEND_ID),
            Self::LlamaCpp | Self::Ollama | Self::Llamafile => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_conversation_category_has_exactly_one_engine() {
        for engine in Engine::ALL {
            assert_eq!(Engine::for_category(&engine.category()), Some(engine));
            assert!(engine.category().is_llm(), "{}", engine.id());
        }
        for other in [
            ModelCategory::Whisper,
            ModelCategory::TtsPiper,
            ModelCategory::TtsKokoro,
            ModelCategory::TtsHttp,
            ModelCategory::Embedding,
        ] {
            assert_eq!(Engine::for_category(&other), None, "{}", other.as_str());
        }
    }

    #[test]
    fn the_serialised_id_is_the_id() {
        for engine in Engine::ALL {
            assert_eq!(
                serde_json::to_value(engine).unwrap(),
                serde_json::json!(engine.id())
            );
        }
    }

    #[test]
    fn only_the_in_process_engines_read_a_file_the_pond_holds_in_memory() {
        assert!(Engine::LlamaCpp.in_process());
        assert!(Engine::LiteRtLm.in_process());
        assert!(!Engine::Ollama.in_process());
        assert!(!Engine::Llamafile.in_process());
        assert_eq!(Engine::LlamaCpp.file_format(), Some(".gguf"));
        assert_eq!(Engine::LiteRtLm.file_format(), Some(".litertlm"));
        assert_eq!(Engine::Ollama.file_format(), None);
    }

    #[test]
    fn only_litert_names_a_goose_backend() {
        assert_eq!(Engine::LiteRtLm.backend_id(), Some("litert"));
        assert_eq!(Engine::LlamaCpp.backend_id(), None);
    }
}
