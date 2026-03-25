//! Whisper GGML model downloader.
//!
//! Downloads quantized Whisper model files from the ggerganov/whisper.cpp
//! HuggingFace repository into GIAP's data directory.
//!
//! These files are loaded by the **whisper.cpp server binary** (a separate
//! process that `WhisperInput` speaks to over HTTP).  This crate only
//! handles the one-time download; it does not run inference itself.

use anyhow::{anyhow, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt as _;

// ── Model registry ────────────────────────────────────────────────────────────

pub struct WhisperModelInfo {
    pub name: &'static str,
    pub filename: &'static str,
    pub url: &'static str,
    /// Approximate size shown during download.
    pub size_mb: u64,
}

/// English-only GGML models ordered by size (smallest → largest).
pub const WHISPER_MODELS: &[WhisperModelInfo] = &[
    WhisperModelInfo {
        name: "tiny",
        filename: "ggml-tiny.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        size_mb: 39,
    },
    WhisperModelInfo {
        name: "base",
        filename: "ggml-base.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        size_mb: 141,
    },
    WhisperModelInfo {
        name: "small",
        filename: "ggml-small.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        size_mb: 244,
    },
];

/// Default model used when none is specified.
pub const DEFAULT_WHISPER_MODEL: &str = "base";

// ── Path helpers ──────────────────────────────────────────────────────────────

pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

/// Returns the on-disk path for a named model (does not check existence).
pub fn model_path(data_dir: &Path, model_name: &str) -> Result<PathBuf> {
    let info = find_model(model_name)?;
    Ok(models_dir(data_dir).join(info.filename))
}

fn find_model(name: &str) -> Result<&'static WhisperModelInfo> {
    WHISPER_MODELS
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| anyhow!("Unknown model '{}'. Available: tiny, base, small", name))
}

// ── Download ──────────────────────────────────────────────────────────────────

/// Download `model_name` into `<data_dir>/models/` with a live progress line.
///
/// Returns the path to the downloaded file.
/// If the file already exists it is returned immediately (no re-download).
pub async fn download_whisper_model(model_name: &str, data_dir: &Path) -> Result<PathBuf> {
    let info = find_model(model_name)?;

    let dir = models_dir(data_dir);
    tokio::fs::create_dir_all(&dir).await?;
    let out_path = dir.join(info.filename);

    if out_path.exists() {
        println!("  ✅ Already downloaded: {}", out_path.display());
        return Ok(out_path);
    }

    println!("  ⬇  {} (~{} MB)", info.filename, info.size_mb);

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| anyhow!("Failed to build HTTP client: {}", e))?;

    let resp = client
        .get(info.url)
        .send()
        .await
        .map_err(|e| anyhow!("Download request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(anyhow!("Server returned {}", resp.status()));
    }

    // Use Content-Length if available, else fall back to known approximate size.
    let total_bytes = resp
        .content_length()
        .unwrap_or(info.size_mb * 1_048_576);

    // Write to a temp file first so we never leave a partial .bin on disk.
    let tmp_path = out_path.with_extension("bin.part");
    let mut file = tokio::fs::File::create(&tmp_path).await?;
    let mut downloaded: u64 = 0;
    let mut resp = resp;

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| anyhow!("Download interrupted: {}", e))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| anyhow!("Write error: {}", e))?;
        downloaded += chunk.len() as u64;

        let pct = (downloaded * 100) / total_bytes.max(1);
        print!(
            "\r  ⬇  {} / {} MB  ({}%)",
            downloaded / 1_048_576,
            total_bytes / 1_048_576,
            pct
        );
        std::io::stdout().flush().ok();
    }

    println!(); // newline after progress line

    // Atomic rename: only visible if fully written
    tokio::fs::rename(&tmp_path, &out_path).await?;
    println!("  ✅ Saved: {}", out_path.display());

    Ok(out_path)
}
