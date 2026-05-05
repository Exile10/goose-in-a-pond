//! Auto-start helper for the whisper.cpp HTTP server.
//!
//! Flow (called by both `run_setup` and `run_server`):
//!   1. Check if whisper-server is already listening on port 9000.
//!   2. If not, look for the binary in `<data_dir>/bin/` or `PATH`.
//!   3. If still not found, attempt to download it via `model_download::download_whisper_binary`.
//!   4. Once located, spawn the process.  The returned `WhisperProcess` guard kills it on drop.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Child;

use crate::model_download;

// ── Guard ─────────────────────────────────────────────────────────────────────

/// Holds the spawned whisper-server child process.  Kills it on drop.
pub struct WhisperProcess(Child);

impl Drop for WhisperProcess {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

// ── Health check ──────────────────────────────────────────────────────────────

/// Returns `true` if something is already serving at `base_url`.
pub async fn is_running(base_url: &str) -> bool {
    reqwest::Client::new()
        .get(base_url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

// ── Binary lookup ─────────────────────────────────────────────────────────────

/// Find the whisper-server binary: `<data_dir>/bin/` first, then `PATH`.
pub fn find_binary(data_dir: &Path) -> Option<PathBuf> {
    // 1. Canonical install location managed by GIAP setup.
    let local = model_download::whisper_binary_path(data_dir);
    if local.exists() {
        return Some(local);
    }

    // 2. Anywhere on PATH.
    #[cfg(windows)]
    let name = "whisper-server.exe";
    #[cfg(not(windows))]
    let name = "whisper-server";

    if let Ok(path_var) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for dir in path_var.split(sep) {
            let candidate = Path::new(dir).join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Spawn `whisper-server -m <model> --port <port> --host 127.0.0.1`.
///
/// Polls every second for up to 30 s waiting for the server to become ready.
async fn spawn(binary: &Path, model: &Path, port: u16) -> Result<WhisperProcess> {
    let child = tokio::process::Command::new(binary)
        .args(["-m", &model.to_string_lossy()])
        .args(["--port", &port.to_string()])
        .args(["--host", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn {}", binary.display()))?;

    let proc = WhisperProcess(child);
    let url = format!("http://127.0.0.1:{}", port);

    for attempt in 1..=30 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if is_running(&url).await {
            return Ok(proc);
        }
        if attempt == 5 {
            println!("  ⏳ Still waiting for whisper.cpp to load the model...");
        }
    }

    println!("  ⚠  whisper.cpp did not respond within 30 s — it may still be loading.");
    Ok(proc)
}

/// Base URL for whisper given a port.
pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

// ── High-level entry point ────────────────────────────────────────────────────

/// Check → find/download → spawn.
///
/// The base port is [`crate::ports::WHISPER`].  If that port is busy,
/// the next port in arithmetic sequence is tried automatically.
///
/// Returns `(Some(guard), port)` if we started the process, or
/// `(None, port)` if it was already running on the base port.
/// Errors are printed as warnings; the returned port is always valid.
pub async fn try_start(data_dir: &Path, model_path: &Path) -> (Option<WhisperProcess>, u16) {
    let base_port = crate::ports::WHISPER;
    let base_url = url_for(base_port);

    if is_running(&base_url).await {
        println!("  🎙  whisper.cpp already running at {}", base_url);
        return (None, base_port);
    }

    // Find the port we will actually spawn on.
    let port = match crate::ports::find_free_port(base_port).await {
        Some(p) => p,
        None => {
            println!("  ⚠  No free port found near {} for whisper.cpp", base_port);
            return (None, base_port);
        }
    };

    // Find or download the binary.
    let binary = match find_binary(data_dir) {
        Some(p) => p,
        None => {
            println!("  📥 whisper-server not found — downloading...");
            match model_download::download_whisper_binary(data_dir).await {
                Ok(p) => p,
                Err(e) => {
                    println!("  ⚠  Could not obtain whisper-server: {}", e);
                    return (None, base_port);
                }
            }
        }
    };

    if !model_path.exists() {
        println!("  ⚠  Whisper model not found at {}", model_path.display());
        println!("     Run `pond-server setup` to download it.");
        return (None, base_port);
    }

    println!("  🎙  Starting whisper.cpp  ({})", binary.display());

    match spawn(&binary, model_path, port).await {
        Ok(proc) => {
            println!("  ✅ whisper.cpp ready on port {}", port);
            (Some(proc), port)
        }
        Err(e) => {
            println!("  ⚠  Failed to start whisper.cpp: {}", e);
            (None, base_port)
        }
    }
}
