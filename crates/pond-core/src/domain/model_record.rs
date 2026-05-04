//! Model catalog domain types.
//!
//! `ModelRecord` is the single source of truth for a model's metadata,
//! covering all five families: LLM (gguf/llamafile/ollama), ASR (whisper),
//! TTS (piper/http), and Embedding (ONNX sentence encoders).
//! Family-specific fields are `Option<_>`.

use serde::{Deserialize, Serialize};

// ── Category ──────────────────────────────────────────────────────────────────

/// Which technology family a model belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCategory {
    /// Local GGUF file loaded in-process via llama-cpp.
    Gguf,
    /// Self-contained llamafile executable (HTTP server on port 8080).
    Llamafile,
    /// Model served by a running Ollama instance.
    Ollama,
    /// Whisper GGUF for speech-to-text.
    Whisper,
    /// Piper TTS — ONNX binary + config file, spawned as subprocess.
    TtsPiper,
    /// HTTP TTS server (OpenAI-compatible /v1/audio/speech).
    TtsHttp,
    /// Sentence embedding model (ONNX, e.g. all-MiniLM-L6-v2 via fastembed).
    Embedding,
}

impl ModelCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gguf => "gguf",
            Self::Llamafile => "llamafile",
            Self::Ollama => "ollama",
            Self::Whisper => "whisper",
            Self::TtsPiper => "tts_piper",
            Self::TtsHttp => "tts_http",
            Self::Embedding => "embedding",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "gguf" => Some(Self::Gguf),
            "llamafile" => Some(Self::Llamafile),
            "ollama" => Some(Self::Ollama),
            "whisper" => Some(Self::Whisper),
            "tts_piper" | "tts" => Some(Self::TtsPiper),
            "tts_http" => Some(Self::TtsHttp),
            "embedding" => Some(Self::Embedding),
            _ => None,
        }
    }

    /// True for LLM models that can be assigned to a chat/think/task role.
    pub fn is_llm(&self) -> bool {
        matches!(self, Self::Gguf | Self::Llamafile | Self::Ollama)
    }

    /// True for speech-to-text models.
    pub fn is_asr(&self) -> bool {
        matches!(self, Self::Whisper)
    }

    /// True for text-to-speech models.
    pub fn is_tts(&self) -> bool {
        matches!(self, Self::TtsPiper | Self::TtsHttp)
    }

    /// True for sentence embedding models.
    pub fn is_embedding(&self) -> bool {
        matches!(self, Self::Embedding)
    }
}

// ── ModelRecord ───────────────────────────────────────────────────────────────

/// Persisted catalog entry for a single model across all supported families.
///
/// The primary key is `id = "{category}/{name}"` — stable across registry refreshes.
/// Family-specific columns are `None` for other families.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRecord {
    /// Stable primary key: `"{category}/{name}"` e.g. `"gguf/llama-3b"`.
    pub id: String,
    pub category: ModelCategory,
    pub name: String,
    /// On-disk filename. `None` for Ollama models (no local file).
    pub filename: Option<String>,
    pub description: String,
    pub size_mb: u64,

    // ── Source / registry ────────────────────────────────────────────────────
    /// Direct download URL.
    pub url: Option<String>,
    /// HuggingFace spec `"owner/repo:quantization"` (GGUF only).
    pub hf_id: Option<String>,

    // ── LLM fields (Gguf / Llamafile / Ollama) ──────────────────────────────
    /// Approximate runtime RAM in MB — critical for <8 GB hardware.
    pub ram_estimate_mb: Option<u64>,
    /// Suggested role: `"chat"` | `"think"` | `"task"`.
    pub recommended_role: Option<String>,
    /// Maximum context tokens supported by this model.
    pub context_length: Option<u32>,
    /// Quantization scheme e.g. `"Q4_K_M"`, `"Q5_K_S"`.
    pub quantization: Option<String>,

    // ── ASR fields (Whisper) ─────────────────────────────────────────────────
    /// Language code e.g. `"en"`, `"multilingual"`.
    pub asr_language: Option<String>,
    /// Whisper model size: `"tiny"` | `"base"` | `"small"` | `"medium"` | `"large"`.
    pub asr_size: Option<String>,

    // ── TTS fields (TtsPiper / TtsHttp) ─────────────────────────────────────
    /// TTS engine: `"piper"` | `"http"`.
    pub tts_engine: Option<String>,
    /// Voice name e.g. `"lessac"`, `"Vivian"`.
    pub tts_voice_name: Option<String>,
    /// Piper: companion `.onnx.json` config filename.
    pub config_filename: Option<String>,
    /// Piper: download URL for the config file.
    pub config_url: Option<String>,
    /// HTTP TTS: base URL e.g. `"http://127.0.0.1:8181"`.
    pub tts_url: Option<String>,
    /// Output sample rate in Hz e.g. `22050`.
    pub sample_rate: Option<u32>,

    // ── State ────────────────────────────────────────────────────────────────
    /// Whether the model file is present on disk.
    pub downloaded: bool,
    /// True for models added by the user (not from the bundled registry).
    pub is_custom: bool,
}

impl ModelRecord {
    /// Canonical primary key for a model.
    pub fn id_for(category: &ModelCategory, name: &str) -> String {
        format!("{}/{}", category.as_str(), name)
    }
}

// ── BinaryRecord ─────────────────────────────────────────────────────────────

/// Metadata for a tool binary (e.g. whisper-server, piper) fetched from the catalog.
///
/// Binary downloads are version-pinned and platform-specific.
/// The catalog (online registry JSON) includes a `tools` section that seeds these records.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BinaryRecord {
    /// Stable identifier, e.g. `"whisper-server"` or `"piper"`.
    pub name: String,
    /// Pinned version string, e.g. `"v1.8.4"`.
    pub version: String,
    /// Per-platform download URLs keyed by `"{os}-{arch}"`,
    /// e.g. `"macos-arm64"`, `"linux-x86_64"`, `"windows-x86_64"`.
    pub platforms: std::collections::HashMap<String, String>,
}

impl BinaryRecord {
    /// Resolve the download URL for the current compile-target platform, if available.
    pub fn url_for_current_platform(&self) -> Option<&str> {
        let key = Self::current_platform_key();
        self.platforms.get(key).map(String::as_str)
    }

    pub fn current_platform_key() -> &'static str {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            "macos-arm64"
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            "macos-x86_64"
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            "linux-x86_64"
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            "linux-aarch64"
        }
        #[cfg(target_os = "windows")]
        {
            "windows-x86_64"
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            "unknown"
        }
    }
}

// ── ModelRoleAssignment ───────────────────────────────────────────────────────

/// Records which model is assigned to a given role.
///
/// Valid roles: `"chat"` | `"think"` | `"task"` | `"asr"` | `"tts"` | `"embedding"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoleAssignment {
    pub role: String,
    pub model_id: String,
}

impl ModelRoleAssignment {
    /// Returns true if `model_category` is a legal match for `role`.
    pub fn category_matches_role(category: &ModelCategory, role: &str) -> bool {
        match role {
            "chat" | "think" | "task" => category.is_llm(),
            "asr" => category.is_asr(),
            "tts" => category.is_tts(),
            "embedding" => category.is_embedding(),
            _ => false,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_roundtrip() {
        let cats = [
            ModelCategory::Gguf,
            ModelCategory::Llamafile,
            ModelCategory::Ollama,
            ModelCategory::Whisper,
            ModelCategory::TtsPiper,
            ModelCategory::TtsHttp,
            ModelCategory::Embedding,
        ];
        for cat in &cats {
            let s = cat.as_str();
            let back = ModelCategory::from_str(s).expect("roundtrip failed");
            assert_eq!(*cat, back, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn category_families() {
        assert!(ModelCategory::Gguf.is_llm());
        assert!(ModelCategory::Llamafile.is_llm());
        assert!(ModelCategory::Ollama.is_llm());
        assert!(!ModelCategory::Whisper.is_llm());
        assert!(ModelCategory::Whisper.is_asr());
        assert!(!ModelCategory::Gguf.is_asr());
        assert!(ModelCategory::TtsPiper.is_tts());
        assert!(ModelCategory::TtsHttp.is_tts());
        assert!(!ModelCategory::Gguf.is_tts());
        assert!(ModelCategory::Embedding.is_embedding());
        assert!(!ModelCategory::Gguf.is_embedding());
        assert!(!ModelCategory::Embedding.is_llm());
        assert!(!ModelCategory::Embedding.is_asr());
        assert!(!ModelCategory::Embedding.is_tts());
    }

    #[test]
    fn id_for_format() {
        assert_eq!(
            ModelRecord::id_for(&ModelCategory::Gguf, "llama-3b"),
            "gguf/llama-3b"
        );
        assert_eq!(
            ModelRecord::id_for(&ModelCategory::Whisper, "base"),
            "whisper/base"
        );
    }

    #[test]
    fn category_matches_role() {
        assert!(ModelRoleAssignment::category_matches_role(
            &ModelCategory::Gguf,
            "chat"
        ));
        assert!(ModelRoleAssignment::category_matches_role(
            &ModelCategory::Llamafile,
            "think"
        ));
        assert!(ModelRoleAssignment::category_matches_role(
            &ModelCategory::Ollama,
            "task"
        ));
        assert!(!ModelRoleAssignment::category_matches_role(
            &ModelCategory::Whisper,
            "chat"
        ));
        assert!(ModelRoleAssignment::category_matches_role(
            &ModelCategory::Whisper,
            "asr"
        ));
        assert!(!ModelRoleAssignment::category_matches_role(
            &ModelCategory::Gguf,
            "asr"
        ));
        assert!(ModelRoleAssignment::category_matches_role(
            &ModelCategory::TtsPiper,
            "tts"
        ));
        assert!(ModelRoleAssignment::category_matches_role(
            &ModelCategory::TtsHttp,
            "tts"
        ));
        assert!(!ModelRoleAssignment::category_matches_role(
            &ModelCategory::Gguf,
            "tts"
        ));
        assert!(ModelRoleAssignment::category_matches_role(
            &ModelCategory::Embedding,
            "embedding"
        ));
        assert!(!ModelRoleAssignment::category_matches_role(
            &ModelCategory::Gguf,
            "embedding"
        ));
        assert!(!ModelRoleAssignment::category_matches_role(
            &ModelCategory::Embedding,
            "chat"
        ));
        assert!(!ModelRoleAssignment::category_matches_role(
            &ModelCategory::Gguf,
            "unknown"
        ));
    }

    #[test]
    fn tts_legacy_alias() {
        // "tts" string in old registry.json maps to TtsPiper
        assert_eq!(
            ModelCategory::from_str("tts"),
            Some(ModelCategory::TtsPiper)
        );
    }
}
