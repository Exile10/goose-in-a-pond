//! Auto-start helper for the Qwen2.5-TTS HTTP server.
//!
//! The `qwen-tts` PyPI package has no CLI entry point on Windows — it is
//! invoked as `python -m qwen_tts serve --port <port>`.
//!
//! Install + start order on first run:
//!   1. Check if `qwen_tts` package is already in the managed venv or PATH Python.
//!   2. If not, find Python (`python3` / `python` / `py`) on PATH.
//!   3. Create a managed venv at `<data_dir>/venv` if it doesn't exist.
//!   4. Run `<venv>/pip install qwen-tts` inside that venv.
//!   5. Spawn `<venv>/python -m qwen_tts serve --port <port>`.
//!   6. Return a guard that kills the process on drop.
//!
//! If Python is not installed at all, instructions are printed and the server
//! falls back to Piper TTS.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Child;

// ── Guard ─────────────────────────────────────────────────────────────────────

/// Holds the spawned `qwen-tts serve` child process. Kills it on drop.
pub struct QwenTtsProcess(Child);

impl Drop for QwenTtsProcess {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

// ── Invocation strategy ───────────────────────────────────────────────────────

/// The two ways we can start the server.
#[derive(Clone)]
enum Invoker {
    /// `<exe> serve --port <port>` — direct CLI entry-point (rare on Windows)
    Exe(PathBuf),
    /// `<python> -m qwen_tts serve --port <port>` — standard module invocation
    Module(PathBuf),
}

impl Invoker {
    fn description(&self) -> String {
        match self {
            Invoker::Exe(p) => p.display().to_string(),
            Invoker::Module(p) => format!("{} -m qwen_tts", p.display()),
        }
    }

    fn into_command(self, port: u16) -> tokio::process::Command {
        let port_str = port.to_string();
        match self {
            Invoker::Exe(exe) => {
                let mut cmd = tokio::process::Command::new(exe);
                cmd.args(["serve", "--port", &port_str]);
                cmd
            }
            Invoker::Module(python) => {
                let mut cmd = tokio::process::Command::new(python);
                cmd.args(["-m", "qwen_tts", "serve", "--port", &port_str]);
                cmd
            }
        }
    }
}

// ── Venv path helpers ─────────────────────────────────────────────────────────

pub fn venv_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("venv")
}

fn venv_scripts_dir(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    return venv_dir(data_dir).join("Scripts");
    #[cfg(not(windows))]
    return venv_dir(data_dir).join("bin");
}

fn venv_python(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    return venv_scripts_dir(data_dir).join("python.exe");
    #[cfg(not(windows))]
    return venv_scripts_dir(data_dir).join("python");
}

fn venv_pip(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    return venv_scripts_dir(data_dir).join("pip.exe");
    #[cfg(not(windows))]
    return venv_scripts_dir(data_dir).join("pip");
}

/// Root site-packages directory inside the managed venv.
fn venv_site_packages(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    return venv_dir(data_dir).join("Lib").join("site-packages");

    #[cfg(not(windows))]
    {
        // lib/pythonX.Y/site-packages — find the first matching subdirectory.
        let lib = venv_dir(data_dir).join("lib");
        if let Ok(entries) = std::fs::read_dir(&lib) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("python") {
                    return entry.path().join("site-packages");
                }
            }
        }
        lib.join("site-packages") // safe fallback
    }
}

/// True if `qwen_tts` package directory exists inside the managed venv.
fn venv_has_qwen_tts(data_dir: &Path) -> bool {
    venv_site_packages(data_dir).join("qwen_tts").exists()
}

// ── Invoker discovery ─────────────────────────────────────────────────────────

/// Find a way to invoke the Qwen TTS server.
///
/// Preference order:
///   1. Managed venv Python + qwen_tts installed (most reliable)
///   2. Direct `qwen-tts` / `qwen_tts` executable in venv Scripts dir
///   3. Same executables anywhere on PATH
fn find_invoker(data_dir: &Path) -> Option<Invoker> {
    // 1. Managed venv: python -m qwen_tts
    let py = venv_python(data_dir);
    if py.exists() && venv_has_qwen_tts(data_dir) {
        return Some(Invoker::Module(py));
    }

    // 2. Direct executable in venv Scripts
    let scripts = venv_scripts_dir(data_dir);
    for base in &["qwen-tts", "qwen_tts"] {
        for candidate in exe_variants(base) {
            let p = scripts.join(&candidate);
            if p.exists() {
                return Some(Invoker::Exe(p));
            }
        }
    }

    // 3. Executable anywhere on PATH
    for base in &["qwen-tts", "qwen_tts"] {
        for candidate in exe_variants(base) {
            if let Some(p) = find_in_path(&candidate) {
                return Some(Invoker::Exe(PathBuf::from(p)));
            }
        }
    }

    None
}

fn exe_variants(base: &str) -> Vec<String> {
    #[cfg(windows)]
    return vec![format!("{}.exe", base), format!("{}.cmd", base), base.to_string()];
    #[cfg(not(windows))]
    return vec![base.to_string()];
}

fn find_in_path(name: &str) -> Option<String> {
    let path_var = std::env::var("PATH").ok()?;
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path_var.split(sep) {
        let p = Path::new(dir).join(name);
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}

fn find_python() -> Option<String> {
    #[cfg(windows)]
    let candidates = &["python.exe", "python3.exe", "py.exe"][..];
    #[cfg(not(windows))]
    let candidates = &["python3", "python"][..];

    for name in candidates {
        if find_in_path(name).is_some() {
            return Some(name.to_string());
        }
    }
    None
}

// ── Health check ──────────────────────────────────────────────────────────────

/// Returns `true` if the Qwen TTS server is already responding at `base_url`.
pub async fn is_running(base_url: &str) -> bool {
    reqwest::Client::new()
        .get(base_url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

// ── Venv creation + pip install ───────────────────────────────────────────────

/// Create managed venv (if absent) and `pip install qwen-tts` inside it.
/// Returns the `Invoker` to use for spawning.
async fn ensure_installed(data_dir: &Path) -> Result<Invoker> {
    let venv = venv_dir(data_dir);

    let python = find_python().ok_or_else(|| {
        anyhow::anyhow!(
            "Python not found — install Python 3.9+ then re-run setup.\n\
             Download: https://www.python.org/downloads/"
        )
    })?;

    // Create venv if absent.
    if !venv.exists() {
        println!("  🐍 Creating Python venv at {} ...", venv.display());
        let status = tokio::process::Command::new(&python)
            .args(["-m", "venv", &venv.to_string_lossy()])
            .status()
            .await
            .context("Failed to run `python -m venv`")?;
        if !status.success() {
            anyhow::bail!("venv creation failed (exit {})", status);
        }
        println!("  ✅ venv created");
    }

    // pip install qwen-tts (idempotent — pip skips if already satisfied)
    let pip = venv_pip(data_dir);
    println!("  📦 Installing qwen-tts into venv...");
    let status = tokio::process::Command::new(&pip)
        .args(["install", "--upgrade", "qwen-tts"])
        .status()
        .await
        .context("Failed to run pip install")?;
    if !status.success() {
        anyhow::bail!("`pip install qwen-tts` failed (exit {})", status);
    }

    find_invoker(data_dir)
        .ok_or_else(|| anyhow::anyhow!("qwen_tts module not found in venv after install"))
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

async fn spawn(invoker: Invoker, port: u16) -> Result<QwenTtsProcess> {
    let desc = invoker.description();
    let mut cmd = invoker.into_command(port);
    let child = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn Qwen TTS via {}", desc))?;

    let proc = QwenTtsProcess(child);
    let url = format!("http://127.0.0.1:{}", port);

    for attempt in 1..=60 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if is_running(&url).await {
            return Ok(proc);
        }
        if attempt == 10 {
            println!("  ⏳ Still waiting for Qwen TTS to load the model (first run can take ~30 s)...");
        }
    }

    println!("  ⚠  Qwen TTS did not respond within 60 s — it may still be loading.");
    Ok(proc)
}

// ── High-level entry points ───────────────────────────────────────────────────

fn extract_port(url: &str) -> u16 {
    url.rsplit(':')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8181)
}

/// Ensure Qwen TTS is installed, then start it if not already running.
/// Never returns `Err` — all failures are printed as warnings.
pub async fn try_start(base_url: &str, data_dir: &Path) -> Option<QwenTtsProcess> {
    if is_running(base_url).await {
        println!("  🔊 Qwen TTS already running at {}", base_url);
        return None;
    }

    let port = extract_port(base_url);

    let invoker = match find_invoker(data_dir) {
        Some(i) => i,
        None => {
            println!("  📦 qwen-tts not found — installing automatically...");
            match ensure_installed(data_dir).await {
                Ok(i) => i,
                Err(e) => {
                    println!("  ⚠  Could not install qwen-tts: {}", e);
                    println!("     Piper will be used as TTS fallback.");
                    return None;
                }
            }
        }
    };

    println!("  🔊 Starting Qwen TTS ({})...", invoker.description());
    match spawn(invoker, port).await {
        Ok(proc) => {
            println!("  ✅ Qwen TTS ready on port {}", port);
            Some(proc)
        }
        Err(e) => {
            println!("  ⚠  Failed to start Qwen TTS: {}", e);
            println!("     Piper will be used as TTS fallback.");
            None
        }
    }
}

/// Install Qwen TTS during `pond-server setup`. Returns `true` if ready.
pub async fn setup_install(data_dir: &Path) -> bool {
    if find_invoker(data_dir).is_some() {
        println!("  ✅ Qwen TTS already installed.");
        return true;
    }
    match ensure_installed(data_dir).await {
        Ok(_)  => true,
        Err(e) => {
            println!("  ⚠  Qwen TTS auto-install failed: {}", e);
            false
        }
    }
}
