//! Auto-start helper for the llamafile LLM server.
//!
//! llamafile bundles model weights + llama.cpp into a single executable.
//! Running it with `--server --port 8080` starts an OpenAI-compatible HTTP
//! server at `http://127.0.0.1:8080/v1/chat/completions`.
//!
//! Flow (called by `run_server`):
//!   1. If something is already answering on port 8080, do nothing.
//!   2. Find the first downloaded llamafile model in `<data_dir>/models/llm/`.
//!   3. Spawn it as a background process.
//!   4. Return a `LlamafileProcess` guard that kills it on drop.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Child;

use crate::model_download;

// ── Guard ─────────────────────────────────────────────────────────────────────

/// Holds the spawned llamafile child process.  Kills it on drop.
pub struct LlamafileProcess(Child);

impl Drop for LlamafileProcess {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

// ── Health check ──────────────────────────────────────────────────────────────

/// Returns `true` if something is already serving on `port`.
pub async fn is_running(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}", port);
    reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

// ── Binary lookup ─────────────────────────────────────────────────────────────

/// Find the first downloaded llamafile model in `<data_dir>/models/llm/`.
///
/// Searches in `LLAMAFILE_MODELS` order (lightest first).
pub fn find_model(data_dir: &Path) -> Option<PathBuf> {
    for info in model_download::LLAMAFILE_MODELS {
        if let Ok(path) = model_download::llamafile_path(data_dir, info.name) {
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Spawn the llamafile server and wait up to 60 s for it to become ready.
///
/// Flags:
///   `--server`        — HTTP server mode (serves `/v1/chat/completions`)
///   `--port <port>`   — listen port
///   `--host 127.0.0.1` — loopback only (local privacy, matches GIAP policy)
///   `--nobrowser`     — don't open a browser tab
///
/// Loading a 1–2 GB model typically takes 5–30 seconds depending on hardware.
async fn spawn(binary: &Path, port: u16) -> Result<LlamafileProcess> {
    let child = tokio::process::Command::new(binary)
        .arg("--server")
        .args(["--port", &port.to_string()])
        .args(["--host", "127.0.0.1"])
        .arg("--nobrowser")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn {}", binary.display()))?;

    let proc = LlamafileProcess(child);

    for attempt in 1..=60 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if is_running(port).await {
            return Ok(proc);
        }
        if attempt == 15 {
            println!("  ⏳ Still loading LLM model — this can take up to 30 s on first launch...");
        }
    }

    // Model still loading after 60 s — return the guard anyway.
    // The first chat request will either succeed (if it finishes loading) or
    // the FallbackProvider will catch the error.
    println!("  ⚠  LLM did not respond within 60 s — it may still be loading in the background.");
    Ok(proc)
}

// ── High-level entry point ────────────────────────────────────────────────────

/// Check → find → spawn.  Never returns an error — failures are printed as warnings.
///
/// Returns `Some(guard)` if we started the process, `None` if it was already
/// running or no model was found.
pub async fn try_start(data_dir: &Path, port: u16) -> Option<LlamafileProcess> {
    if is_running(port).await {
        println!("  🧠 LLM already running at http://127.0.0.1:{}", port);
        return None;
    }

    let model = match find_model(data_dir) {
        Some(p) => p,
        None => {
            println!("  ⚠  No LLM model found — run `pond-server setup` to download one.");
            println!("     Chat will fall back to mock echo until a model is available.");
            return None;
        }
    };

    println!(
        "  🧠 Starting llamafile  ({})...",
        model.file_name().unwrap_or_default().to_string_lossy()
    );

    match spawn(&model, port).await {
        Ok(proc) => {
            println!("  ✅ LLM ready on port {}", port);
            Some(proc)
        }
        Err(e) => {
            println!("  ⚠  Failed to start llamafile: {}", e);
            None
        }
    }
}
