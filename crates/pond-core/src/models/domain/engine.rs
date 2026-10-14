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

/// Why this pond cannot run an engine, as the wire names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineUnavailable {
    /// LiteRT-LM's C API library is in none of the places goose's `litert` backend searches, so
    /// every `.litertlm` model would fail on its first turn.
    #[serde(rename = "litert_library_missing")]
    LiteRtLibraryMissing,
}

impl EngineUnavailable {
    /// The serialised reason.
    pub fn id(self) -> &'static str {
        match self {
            Self::LiteRtLibraryMissing => "litert_library_missing",
        }
    }
}

/// Which engines this pond's process can run, decided once at startup by the server, which is
/// the only layer that can look for the libraries an engine loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineAvailability {
    /// Whether a LiteRT-LM library is where goose's `litert` backend will look for it.
    pub litert_library: bool,
}

impl EngineAvailability {
    /// Why `engine` cannot run here; `None` when it can.
    pub fn unavailable(self, engine: Engine) -> Option<EngineUnavailable> {
        match engine {
            Engine::LiteRtLm if !self.litert_library => {
                Some(EngineUnavailable::LiteRtLibraryMissing)
            }
            Engine::LiteRtLm | Engine::LlamaCpp | Engine::Ollama | Engine::Llamafile => None,
        }
    }

    /// Why a model of `category` cannot run here; `None` for one that can, or that no
    /// conversation engine runs.
    pub fn unavailable_for(self, category: &ModelCategory) -> Option<EngineUnavailable> {
        Engine::for_category(category).and_then(|engine| self.unavailable(engine))
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

    #[test]
    fn without_the_litert_library_only_litert_models_are_unavailable() {
        let missing = EngineAvailability {
            litert_library: false,
        };
        assert_eq!(
            missing.unavailable(Engine::LiteRtLm),
            Some(EngineUnavailable::LiteRtLibraryMissing)
        );
        assert_eq!(
            missing.unavailable_for(&ModelCategory::Litert),
            Some(EngineUnavailable::LiteRtLibraryMissing)
        );
        for engine in [Engine::LlamaCpp, Engine::Ollama, Engine::Llamafile] {
            assert_eq!(missing.unavailable(engine), None, "{}", engine.id());
        }
        assert_eq!(missing.unavailable_for(&ModelCategory::Whisper), None);
    }

    #[test]
    fn with_the_litert_library_every_engine_is_available() {
        let present = EngineAvailability {
            litert_library: true,
        };
        for engine in Engine::ALL {
            assert_eq!(present.unavailable(engine), None, "{}", engine.id());
        }
    }

    #[test]
    fn the_serialised_reason_is_the_id() {
        let reason = EngineUnavailable::LiteRtLibraryMissing;
        assert_eq!(
            serde_json::to_value(reason).unwrap(),
            serde_json::json!(reason.id())
        );
    }
}
