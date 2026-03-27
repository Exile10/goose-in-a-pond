//! Model registry — loads available models from a bundled JSON file
//! and optionally refreshes it from an online URL.
//!
//! The registry lists all downloadable models in three categories:
//! - Whisper ASR models
//! - Llamafile LLM models
//! - TTS models (HTTP-based or Piper subprocess)
//!
//! On startup the server loads the cached registry from disk, falling back
//! to the bundled compile-time copy if the cache is absent or corrupt.

use anyhow::{Context, Result};
use pond_api::ModelStatusEntry;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Compile-time fallback — always available offline.
const BUNDLED_REGISTRY: &str = include_str!("../registry.json");
const REGISTRY_CACHE_FILE: &str = "registry.json";

// ── Registry structs ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRegistry {
    pub version: String,
    pub whisper: Vec<WhisperModelEntry>,
    pub llamafile: Vec<LlamafileModelEntry>,
    pub tts: Vec<TtsModelEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhisperModelEntry {
    pub name: String,
    pub filename: String,
    pub url: String,
    pub size_mb: u64,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlamafileModelEntry {
    pub name: String,
    pub filename: String,
    pub url: String,
    pub size_mb: u64,
    pub description: String,
}

/// A TTS model entry — either an HTTP server or a local Piper subprocess.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "snake_case")]
pub enum TtsModelEntry {
    /// OpenAI-compatible TTS HTTP server (e.g. Qwen2.5-TTS).
    Http {
        name: String,
        default_url: String,
        size_mb: u64,
        description: String,
        setup_hint: Option<String>,
    },
    /// Local Piper subprocess TTS (binary + ONNX model auto-downloaded).
    Piper {
        name: String,
        model_filename: String,
        config_filename: String,
        model_url: String,
        config_url: String,
        size_mb: u64,
        description: String,
    },
}

impl TtsModelEntry {
    pub fn name(&self) -> &str {
        match self {
            TtsModelEntry::Http  { name, .. } => name,
            TtsModelEntry::Piper { name, .. } => name,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            TtsModelEntry::Http  { description, .. } => description,
            TtsModelEntry::Piper { description, .. } => description,
        }
    }

    pub fn size_mb(&self) -> u64 {
        match self {
            TtsModelEntry::Http  { size_mb, .. } => *size_mb,
            TtsModelEntry::Piper { size_mb, .. } => *size_mb,
        }
    }
}

// ── ModelRegistry impl ────────────────────────────────────────────────────────

impl ModelRegistry {
    /// Load the compile-time bundled registry. Always succeeds.
    pub fn load_bundled() -> Self {
        serde_json::from_str(BUNDLED_REGISTRY)
            .expect("bundled registry.json failed to parse — this is a build error")
    }

    /// Load from the on-disk cache in `data_dir`, falling back to bundled.
    pub fn load_cached(data_dir: &Path) -> Self {
        let cache = data_dir.join(REGISTRY_CACHE_FILE);
        if let Ok(bytes) = std::fs::read(&cache) {
            if let Ok(r) = serde_json::from_slice::<ModelRegistry>(&bytes) {
                return r;
            }
        }
        Self::load_bundled()
    }

    /// Fetch the latest registry from `url`, save it to `data_dir`, and return it.
    /// Falls back to the cached/bundled registry on any network or parse error.
    pub async fn fetch_and_cache(url: &str, data_dir: &Path) -> Result<Self> {
        let bytes = reqwest::get(url)
            .await
            .context("Failed to fetch registry")?
            .bytes()
            .await
            .context("Failed to read registry response")?;

        let registry: ModelRegistry =
            serde_json::from_slice(&bytes).context("Registry JSON is invalid")?;

        // Cache to disk (best-effort — don't fail if write fails)
        let cache = data_dir.join(REGISTRY_CACHE_FILE);
        let _ = std::fs::write(&cache, &bytes);

        Ok(registry)
    }

    // ── Lookup helpers ────────────────────────────────────────────────────────

    pub fn find_whisper(&self, name: &str) -> Option<&WhisperModelEntry> {
        self.whisper.iter().find(|m| m.name == name)
    }

    pub fn find_llamafile(&self, name: &str) -> Option<&LlamafileModelEntry> {
        self.llamafile.iter().find(|m| m.name == name)
    }

    pub fn find_tts(&self, name: &str) -> Option<&TtsModelEntry> {
        self.tts.iter().find(|m| m.name() == name)
    }
}

/// Build a status snapshot by checking which model files are present on disk.
pub fn build_model_status(
    registry: &ModelRegistry,
    active_whisper: &str,
    active_llamafile: &str,
    active_tts: &str,
    data_dir: &Path,
) -> Vec<ModelStatusEntry> {
    let mut out = Vec::new();

    for m in &registry.whisper {
        let path = data_dir.join("models").join(&m.filename);
        out.push(ModelStatusEntry {
            category: "whisper".into(),
            name: m.name.clone(),
            description: m.description.clone(),
            size_mb: m.size_mb,
            downloaded: path.exists(),
            active: m.name == active_whisper,
        });
    }

    for m in &registry.llamafile {
        // On Windows the file has a .exe extension.
        let base = data_dir.join("models").join("llm").join(&m.filename);
        #[cfg(windows)]
        let path = PathBuf::from(format!("{}.exe", base.display()));
        #[cfg(not(windows))]
        let path = base;
        out.push(ModelStatusEntry {
            category: "llamafile".into(),
            name: m.name.clone(),
            description: m.description.clone(),
            size_mb: m.size_mb,
            downloaded: path.exists(),
            active: m.name == active_llamafile,
        });
    }

    for m in &registry.tts {
        let downloaded = match m {
            TtsModelEntry::Http { .. } => true, // server-side — always "available"
            TtsModelEntry::Piper { model_filename, .. } => {
                data_dir.join("models").join("tts").join(model_filename).exists()
            }
        };
        out.push(ModelStatusEntry {
            category: "tts".into(),
            name: m.name().to_string(),
            description: m.description().to_string(),
            size_mb: m.size_mb(),
            downloaded,
            active: m.name() == active_tts,
        });
    }

    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_registry_parses_successfully() {
        let r = ModelRegistry::load_bundled();
        assert!(!r.whisper.is_empty(), "whisper list must not be empty");
        assert!(!r.llamafile.is_empty(), "llamafile list must not be empty");
        assert!(!r.tts.is_empty(), "tts list must not be empty");
    }

    #[test]
    fn find_helpers_return_correct_entries() {
        let r = ModelRegistry::load_bundled();
        assert!(r.find_whisper("base").is_some());
        assert!(r.find_whisper("nonexistent").is_none());
        assert!(r.find_llamafile("gemma-2b").is_some());
        assert!(r.find_tts("qwen-tts").is_some());
        assert!(r.find_tts("piper-lessac").is_some());
    }

    #[test]
    fn tts_entry_name_accessor() {
        let r = ModelRegistry::load_bundled();
        for entry in &r.tts {
            assert!(!entry.name().is_empty());
        }
    }

    #[test]
    fn load_cached_falls_back_to_bundled_when_no_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let r = ModelRegistry::load_cached(tmp.path());
        // Should return bundled without panicking.
        assert!(!r.whisper.is_empty());
    }

    #[test]
    fn build_model_status_marks_active_correctly() {
        let r = ModelRegistry::load_bundled();
        let tmp = tempfile::tempdir().unwrap();
        let status = build_model_status(&r, "base", "gemma-2b", "qwen-tts", tmp.path());

        let whisper_active: Vec<_> = status.iter()
            .filter(|s| s.category == "whisper" && s.active)
            .collect();
        assert_eq!(whisper_active.len(), 1);
        assert_eq!(whisper_active[0].name, "base");

        let tts_active: Vec<_> = status.iter()
            .filter(|s| s.category == "tts" && s.active)
            .collect();
        assert_eq!(tts_active.len(), 1);
        assert_eq!(tts_active[0].name, "qwen-tts");
    }
}
