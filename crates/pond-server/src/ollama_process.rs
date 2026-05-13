//! Auto-start helper for the Ollama LLM server.
//!
//! Ollama may be installed as a system service (systemd/launchd) or
//! run manually via `ollama serve`.  This module detects, starts, and
//! monitors the Ollama process so GIAP users never need to manually
//! launch it.
//!
//! Unlike llamafile, Ollama is **not killed on drop** — it may be a
//! system service shared with other applications.  We only start it,
//! never stop it.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Default Ollama HTTP port.
const OLLAMA_PORT: u16 = 11434;

/// Version endpoint used for health checking.
const VERSION_PATH: &str = "/api/version";

// ── Health check ──────────────────────────────────────────────────────────────

/// Returns `true` if Ollama is responding on `127.0.0.1:11434`.
pub async fn is_running() -> bool {
    let url = format!("http://127.0.0.1:{}{}", OLLAMA_PORT, VERSION_PATH);
    reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

// ── Binary lookup ─────────────────────────────────────────────────────────────

/// Find the `ollama` binary on the system.
///
/// Checks `PATH` first via `which`, then well-known macOS install locations.
pub fn find_binary() -> Option<PathBuf> {
    // Try `which ollama` — works on all platforms when ollama is in PATH.
    if let Ok(output) = std::process::Command::new("which").arg("ollama").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout);
            let path = PathBuf::from(path_str.trim());
            if path.exists() {
                return Some(path);
            }
        }
    }

    // Well-known macOS install locations.
    #[cfg(target_os = "macos")]
    {
        let candidates = ["/usr/local/bin/ollama", "/opt/homebrew/bin/ollama"];
        for candidate in &candidates {
            let p = PathBuf::from(candidate);
            if p.exists() {
                return Some(p);
            }
        }
    }

    // Well-known Linux install locations.
    #[cfg(target_os = "linux")]
    {
        let candidates = ["/usr/local/bin/ollama", "/usr/bin/ollama"];
        for candidate in &candidates {
            let p = PathBuf::from(candidate);
            if p.exists() {
                return Some(p);
            }
        }
    }

    None
}

// ── Start service ─────────────────────────────────────────────────────────────

/// Maximum seconds to wait for Ollama to become ready after starting.
const STARTUP_TIMEOUT_SECS: u64 = 30;

/// Start the Ollama server and wait up to [`STARTUP_TIMEOUT_SECS`] for it to
/// become ready.
///
/// On Linux with systemd, tries `systemctl start ollama` first.
/// On macOS and other platforms, spawns `ollama serve` as a background process
/// with stdout/stderr redirected to null.
async fn start_service(binary: &std::path::Path) -> Result<()> {
    // On Linux, try systemd first — many installs configure Ollama as a
    // service and systemd manages the lifecycle (restart, logging, etc.).
    #[cfg(target_os = "linux")]
    {
        let systemctl = tokio::process::Command::new("systemctl")
            .args(["start", "ollama"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;

        if let Ok(status) = systemctl {
            if status.success() {
                tracing::info!("Started Ollama via systemctl");
                return wait_for_ready().await;
            }
            tracing::debug!(
                "systemctl start ollama failed (exit {}); falling back to direct spawn",
                status.code().unwrap_or(-1)
            );
        }
    }

    // Direct spawn: `ollama serve` as a detached background process.
    // stdout/stderr are piped to null so the GIAP server's output stays clean.
    let _child = tokio::process::Command::new(binary)
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn {}", binary.display()))?;

    // Note: we intentionally do NOT hold the Child handle.  Ollama may be a
    // system-wide service — dropping the handle lets the process outlive GIAP.

    tracing::info!("Spawned `ollama serve` (pid will detach)");
    wait_for_ready().await
}

/// Poll `is_running()` once per second until the server responds or
/// [`STARTUP_TIMEOUT_SECS`] elapses.
async fn wait_for_ready() -> Result<()> {
    for attempt in 1..=STARTUP_TIMEOUT_SECS {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if is_running().await {
            tracing::info!("Ollama ready after ~{attempt}s");
            return Ok(());
        }
        if attempt == 15 {
            tracing::info!(
                "Still waiting for Ollama to start — this can take up to 30 s on first launch"
            );
        }
    }
    anyhow::bail!(
        "Ollama did not respond within {STARTUP_TIMEOUT_SECS}s — \
         check `ollama serve` output for errors"
    )
}

// ── High-level entry point ────────────────────────────────────────────────────

/// Ensure Ollama is running, starting it if necessary.
///
/// Returns `Ok(true)` when we started it, `Ok(false)` when it was already
/// running.  Returns `Err` when the binary is not found or startup fails.
pub async fn ensure_running() -> Result<bool> {
    if is_running().await {
        return Ok(false);
    }

    let binary = find_binary()
        .context("Ollama binary not found. Install it: brew install ollama (macOS) or curl -fsSL https://ollama.com/install.sh | sh (Linux)")?;

    tracing::info!(binary = %binary.display(), "Starting Ollama");
    start_service(&binary).await?;
    Ok(true)
}

// ── OllamaManager implementation ─────────────────────────────────────────────

/// Concrete implementation of [`pond_api::OllamaManager`].
///
/// Stateless — Ollama manages its own model state and process lifecycle.
/// This struct is a thin wrapper that calls the module-level functions
/// through the trait interface expected by `AppState`.
pub struct OllamaProcessManager;

impl OllamaProcessManager {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl pond_api::OllamaManager for OllamaProcessManager {
    async fn ensure_started(&self) -> bool {
        match ensure_running().await {
            Ok(started) => {
                if started {
                    tracing::info!("Ollama auto-started successfully");
                }
                true
            }
            Err(e) => {
                tracing::warn!("Failed to start Ollama: {e:#}");
                false
            }
        }
    }

    async fn is_running(&self) -> bool {
        is_running().await
    }

    async fn ensure_started_and_wait(&self, timeout_secs: u64) -> bool {
        // Fast path: already answering.
        if is_running().await {
            return true;
        }

        // Try to start.
        match ensure_running().await {
            Ok(_) => return true, // wait_for_ready already polled inside ensure_running
            Err(e) => {
                tracing::warn!("Ollama startup failed: {e:#}");
            }
        }

        // If ensure_running failed, give it extra time — it might still be
        // loading (e.g. systemd restart race).  Poll until timeout.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if is_running().await {
                return true;
            }
        }
        false
    }
}
