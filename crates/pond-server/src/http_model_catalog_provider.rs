//! HTTP implementation of `ModelCatalogProvider`.
//!
//! Fetches the registry JSON from any URL and converts it into `ModelRecord`s
//! and `BinaryRecord`s.  This is the single place that knows how to parse the
//! registry JSON format — replacing `model_registry.rs`, `model_seed.rs`,
//! and the `ModelRegistrySnapshot` struct in `routes.rs`.
//!
//! # Registry JSON format (v2)
//! ```json
//! {
//!   "version": "2",
//!   "tools": [
//!     { "name": "whisper-server", "version": "v1.8.4",
//!       "platforms": { "macos-arm64": "...", "linux-x86_64": "...", "windows-x86_64": "..." } }
//!   ],
//!   "whisper":   [{ "name", "filename", "url", "size_mb", "description" }],
//!   "llamafile": [{ "name", "filename", "url", "size_mb", "description",
//!                   "ram_estimate_mb"?, "recommended_role"? }],
//!   "tts": [
//!     { "engine": "http",  "name", "default_url", "size_mb", "description" },
//!     { "engine": "piper", "name", "model_filename", "config_filename",
//!                          "model_url", "config_url", "size_mb", "description" }
//!   ],
//!   "gguf": [{ "id" (HF spec), "name", "filename", "url", "size_mb", "description",
//!              "ram_estimate_mb"?, "recommended_role"? }]
//! }
//! ```

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;

use pond_core::domain::model_record::{BinaryRecord, ModelCategory, ModelRecord};
use pond_core::ports::model_catalog_provider::ModelCatalogProvider;

pub struct HttpModelCatalogProvider {
    client: reqwest::Client,
}

impl HttpModelCatalogProvider {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent(concat!("goose-in-a-pond/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client build failed"),
        }
    }
}

#[async_trait]
impl ModelCatalogProvider for HttpModelCatalogProvider {
    async fn fetch(&self, url: &str) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)> {
        let resp = self.client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to fetch catalog from {url}"))?;

        if !resp.status().is_success() {
            anyhow::bail!("Catalog fetch returned {}: {}", resp.status(), url);
        }

        let json: Value = resp.json()
            .await
            .with_context(|| format!("Catalog at {url} is not valid JSON"))?;

        let models   = parse_models(&json);
        let binaries = parse_binaries(&json);

        Ok((models, binaries))
    }
}

// ── JSON → ModelRecord ────────────────────────────────────────────────────────

fn parse_models(json: &Value) -> Vec<ModelRecord> {
    let mut out = Vec::new();
    out.extend(parse_whisper(json));
    out.extend(parse_llamafile(json));
    out.extend(parse_tts(json));
    out.extend(parse_gguf(json));
    out
}

fn parse_whisper(json: &Value) -> Vec<ModelRecord> {
    let Some(arr) = json["whisper"].as_array() else { return vec![] };
    arr.iter().filter_map(|m| {
        let name        = m["name"].as_str()?.to_string();
        let filename    = m["filename"].as_str()?.to_string();
        let url         = m["url"].as_str()?.to_string();
        let size_mb     = m["size_mb"].as_u64().unwrap_or(0);
        let description = m["description"].as_str().unwrap_or("").to_string();
        Some(ModelRecord {
            id:               ModelRecord::id_for(&ModelCategory::Whisper, &name),
            category:         ModelCategory::Whisper,
            name:             name.clone(),
            filename:         Some(filename),
            description,
            size_mb,
            url:              Some(url),
            hf_id:            None,
            ram_estimate_mb:  None,
            recommended_role: None,
            context_length:   None,
            quantization:     None,
            asr_language:     Some(m["language"].as_str().unwrap_or("en").to_string()),
            asr_size:         Some(name),
            tts_engine:       None,
            tts_voice_name:   None,
            config_filename:  None,
            config_url:       None,
            tts_url:          None,
            sample_rate:      None,
            downloaded:       false, // set by ModelService from ModelStorage::is_present
            is_custom:        false,
        })
    }).collect()
}

fn parse_llamafile(json: &Value) -> Vec<ModelRecord> {
    let Some(arr) = json["llamafile"].as_array() else { return vec![] };
    arr.iter().filter_map(|m| {
        let name        = m["name"].as_str()?.to_string();
        let filename    = m["filename"].as_str()?.to_string();
        let url         = m["url"].as_str()?.to_string();
        let size_mb     = m["size_mb"].as_u64().unwrap_or(0);
        let description = m["description"].as_str().unwrap_or("").to_string();
        Some(ModelRecord {
            id:               ModelRecord::id_for(&ModelCategory::Llamafile, &name),
            category:         ModelCategory::Llamafile,
            name,
            filename:         Some(filename),
            description,
            size_mb,
            url:              Some(url),
            hf_id:            None,
            ram_estimate_mb:  m["ram_estimate_mb"].as_u64(),
            recommended_role: m["recommended_role"].as_str().map(|s| s.to_string()),
            context_length:   None,
            quantization:     None,
            asr_language:     None,
            asr_size:         None,
            tts_engine:       None,
            tts_voice_name:   None,
            config_filename:  None,
            config_url:       None,
            tts_url:          None,
            sample_rate:      None,
            downloaded:       false,
            is_custom:        false,
        })
    }).collect()
}

fn parse_tts(json: &Value) -> Vec<ModelRecord> {
    let Some(arr) = json["tts"].as_array() else { return vec![] };
    arr.iter().filter_map(|entry| {
        let name   = entry["name"].as_str()?.to_string();
        let engine = entry["engine"].as_str().unwrap_or("").to_string();

        let (category, filename, config_filename, config_url, url, tts_url) =
            if engine == "http" || engine == "qwen" {
                let default_url = entry["default_url"].as_str().map(|s| s.to_string());
                (ModelCategory::TtsHttp, None, None, None, None, default_url)
            } else {
                // piper
                let fname  = entry["model_filename"].as_str().map(|s| s.to_string());
                let cfname = entry["config_filename"].as_str().map(|s| s.to_string());
                let model_url = entry["model_url"].as_str().map(|s| s.to_string());
                let curl   = entry["config_url"].as_str().map(|s| s.to_string());
                (ModelCategory::TtsPiper, fname, cfname, curl, model_url, None)
            };

        let downloaded = matches!(category, ModelCategory::TtsHttp); // HTTP TTS is always "available"

        Some(ModelRecord {
            id:               ModelRecord::id_for(&category, &name),
            category,
            name:             name.clone(),
            filename,
            description:      entry["description"].as_str().unwrap_or("").to_string(),
            size_mb:          entry["size_mb"].as_u64().unwrap_or(0),
            url,
            hf_id:            None,
            ram_estimate_mb:  None,
            recommended_role: None,
            context_length:   None,
            quantization:     None,
            asr_language:     None,
            asr_size:         None,
            tts_engine:       Some(engine),
            tts_voice_name:   entry["voice"].as_str().map(|s| s.to_string()),
            config_filename,
            config_url,
            tts_url,
            sample_rate:      entry["sample_rate"].as_u64().map(|v| v as u32),
            downloaded,
            is_custom:        false,
        })
    }).collect()
}

fn parse_gguf(json: &Value) -> Vec<ModelRecord> {
    let Some(arr) = json["gguf"].as_array() else { return vec![] };
    arr.iter().filter_map(|m| {
        let name     = m["name"].as_str()?.to_string();
        let filename = m["filename"].as_str().map(|s| s.to_string());
        Some(ModelRecord {
            id:               ModelRecord::id_for(&ModelCategory::Gguf, &name),
            category:         ModelCategory::Gguf,
            name,
            filename,
            description:      m["description"].as_str().unwrap_or("").to_string(),
            size_mb:          m["size_mb"].as_u64().unwrap_or(0),
            url:              m["url"].as_str().map(|s| s.to_string()),
            hf_id:            m["id"].as_str().map(|s| s.to_string()),
            ram_estimate_mb:  m["ram_estimate_mb"].as_u64(),
            recommended_role: m["recommended_role"].as_str().map(|s| s.to_string()),
            context_length:   m["context_length"].as_u64().map(|v| v as u32),
            quantization:     m["quantization"].as_str().map(|s| s.to_string()),
            asr_language:     None,
            asr_size:         None,
            tts_engine:       None,
            tts_voice_name:   None,
            config_filename:  None,
            config_url:       None,
            tts_url:          None,
            sample_rate:      None,
            downloaded:       false,
            is_custom:        false,
        })
    }).collect()
}

// ── JSON → BinaryRecord ───────────────────────────────────────────────────────

fn parse_binaries(json: &Value) -> Vec<BinaryRecord> {
    let Some(arr) = json["tools"].as_array() else { return vec![] };
    arr.iter().filter_map(|t| {
        let name    = t["name"].as_str()?.to_string();
        let version = t["version"].as_str().unwrap_or("").to_string();
        let platforms = t["platforms"].as_object()?.iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect();
        Some(BinaryRecord { name, version, platforms })
    }).collect()
}
