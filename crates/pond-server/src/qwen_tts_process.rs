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
//! If Python is unavailable or install fails, falls back to Piper TTS.

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

/// Return the path to the first Python 3.10+ interpreter found on PATH.
/// Checks the actual version for generic names like `python3` / `python`.
fn find_python() -> Option<String> {
    #[cfg(windows)]
    let candidates: &[&str] = &[
        "python3.13.exe", "python3.12.exe", "python3.11.exe", "python3.10.exe",
        "python3.exe", "python.exe", "py.exe",
    ];
    #[cfg(not(windows))]
    let candidates: &[&str] = &[
        "python3.13", "python3.12", "python3.11", "python3.10",
        "python3", "python",
    ];

    for &name in candidates {
        let Some(path) = find_in_path(name) else { continue };
        // For versioned names (python3.10 etc.) we trust the name.
        // For generic names verify the actual interpreter version.
        let needs_check = !name.contains("3.10")
            && !name.contains("3.11")
            && !name.contains("3.12")
            && !name.contains("3.13");
        if needs_check {
            // Run a quick version probe (blocking, <10 ms).
            let ok = std::process::Command::new(&path)
                .args(["-c", "import sys; v=sys.version_info; exit(0 if (v.major,v.minor)>=(3,10) else 1)"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                continue;
            }
        }
        return Some(path);
    }
    None
}

/// Find a Python 3.10+ interpreter, installing one automatically if needed.
///
/// On macOS without a suitable Python: `brew install python@3.12`.
/// On Linux: tries apt/dnf.
/// Returns the interpreter path, or `None` if all attempts fail.
async fn find_or_install_python() -> Option<String> {
    if let Some(p) = find_python() {
        return Some(p);
    }

    println!("  🐍 No Python 3.10+ found — installing Python 3.12...");

    #[cfg(target_os = "macos")]
    {
        let _ = tokio::process::Command::new("brew")
            .args(["install", "python@3.12"])
            .status()
            .await;
        // brew links python3.12 into its prefix bin directory.
        for candidate in &[
            "/opt/homebrew/bin/python3.12",
            "/usr/local/bin/python3.12",
        ] {
            if std::path::Path::new(candidate).exists() {
                return Some(candidate.to_string());
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        for args in &[
            &["apt-get", "install", "-y", "python3.12"][..],
            &["apt",     "install", "-y", "python3.12"],
            &["dnf",     "install", "-y", "python3.12"],
            &["yum",     "install", "-y", "python3.12"],
        ] {
            if tokio::process::Command::new("sudo")
                .args(*args)
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
            {
                break;
            }
        }
    }

    // Re-probe after install attempt.
    find_python()
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

/// Returns the (major, minor) Python version running inside the managed venv, if available.
async fn venv_python_version(data_dir: &Path) -> Option<(u32, u32)> {
    let python = venv_python(data_dir);
    if !python.exists() {
        return None;
    }
    let out = tokio::process::Command::new(&python)
        .args(["-c", "import sys; print(sys.version_info.major, sys.version_info.minor)"])
        .output()
        .await
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut parts = s.split_whitespace();
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Create managed venv (if absent) and install qwen-tts with all prerequisites.
/// Returns the `Invoker` to use for spawning.
async fn ensure_installed(data_dir: &Path) -> Result<Invoker> {
    let venv = venv_dir(data_dir);

    let python = find_or_install_python()
        .await
        .ok_or_else(|| anyhow::anyhow!("Python 3.10+ unavailable and auto-install failed"))?;

    // Install system sox binary — required by the sox Python package's setup.py.
    install_system_sox().await;

    // If the venv exists but was created with Python < 3.10, delete and recreate it.
    // accelerate (a qwen-tts dependency) requires Python 3.10+.
    if venv.exists() {
        if let Some((maj, min)) = venv_python_version(data_dir).await {
            if maj < 3 || (maj == 3 && min < 10) {
                println!("  🔄 Recreating venv (Python {}.{} < 3.10; accelerate requires 3.10+)...", maj, min);
                tokio::fs::remove_dir_all(&venv).await.ok();
            }
        }
    }

    // Create venv if absent (or just deleted above).
    if !venv.exists() {
        println!("  🐍 Creating Python venv ({})...", python);
        let status = tokio::process::Command::new(&python)
            .args(["-m", "venv", &venv.to_string_lossy()])
            .status()
            .await
            .context("Failed to run `python -m venv`")?;
        if !status.success() {
            anyhow::bail!("venv creation failed (exit {})", status);
        }
    }

    let pip = venv_pip(data_dir);

    // Upgrade pip — old pip (e.g. 21.x bundled with Xcode CLT Python) has
    // poor dependency resolver behaviour that causes false conflicts.
    let _ = tokio::process::Command::new(&pip)
        .args(["install", "--upgrade", "pip"])
        .status()
        .await;

    // numpy must be installed before sox (the Python package) because sox's
    // setup.py imports numpy during metadata collection.
    let _ = tokio::process::Command::new(&pip)
        .args(["install", "numpy"])
        .status()
        .await;

    // Install qwen-tts. Do not pass --upgrade to avoid pulling in conflicting
    // transitive upgrades on top of an existing environment.
    println!("  📦 Installing qwen-tts...");
    let status = tokio::process::Command::new(&pip)
        .args(["install", "qwen-tts"])
        .status()
        .await
        .context("Failed to run pip install")?;
    if !status.success() {
        anyhow::bail!("`pip install qwen-tts` failed (exit {})", status);
    }

    find_invoker(data_dir)
        .ok_or_else(|| anyhow::anyhow!("qwen_tts module not found in venv after install"))
}

/// Install the system `sox` audio tool if it is not already present.
/// Required by the `sox` Python package which qwen-tts depends on.
async fn install_system_sox() {
    // Check if already installed.
    let already = tokio::process::Command::new("sox")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    if already { return; }

    println!("  📥 Installing system sox...");

    #[cfg(target_os = "macos")]
    {
        let _ = tokio::process::Command::new("brew")
            .args(["install", "sox"])
            .status().await;
    }

    #[cfg(target_os = "linux")]
    {
        for args in &[
            &["apt-get", "install", "-y", "sox"][..],
            &["apt",     "install", "-y", "sox"],
            &["dnf",     "install", "-y", "sox"],
            &["yum",     "install", "-y", "sox"],
            &["pacman",  "--noconfirm", "-S", "sox"],
        ] {
            if tokio::process::Command::new("sudo")
                .args(*args)
                .status().await.map(|s| s.success()).unwrap_or(false)
            { break; }
        }
    }
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Spawns the Qwen TTS process and waits up to 60 s for it to respond.
/// Returns `(process, confirmed)` where `confirmed` is `true` only when
/// the server actually responded within the timeout window.
async fn spawn(invoker: Invoker, port: u16) -> Result<(QwenTtsProcess, bool)> {
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
            return Ok((proc, true));
        }
        if attempt == 10 {
            println!("  ⏳ Still waiting for Qwen TTS to load the model (first run can take ~30 s)...");
        }
    }

    println!("  ⚠  Qwen TTS did not respond within 60 s — it may still be loading in the background.");
    Ok((proc, false))
}

// ── High-level entry points ───────────────────────────────────────────────────

/// Base URL for Qwen TTS given a port.
pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

/// Ensure Qwen TTS is installed, then start it if not already running.
///
/// The base port is [`crate::ports::QWEN_TTS`].  If busy, the next free port
/// in arithmetic sequence is used automatically.
///
/// Returns `Some((process, actual_port, confirmed))` where `confirmed` is
/// `true` when the server responded within the startup window.  Returns `None`
/// when already running (caller should re-check [`is_running`]) or on failure.
pub async fn try_start(data_dir: &Path) -> Option<(QwenTtsProcess, u16, bool)> {
    let base_port = crate::ports::QWEN_TTS;
    let base_url = url_for(base_port);

    if is_running(&base_url).await {
        println!("  🔊 Qwen TTS already running at {}", base_url);
        return None;
    }

    let port = match crate::ports::find_free_port(base_port).await {
        Some(p) => p,
        None => {
            println!(
                "  ⚠  No free port found near {} for Qwen TTS",
                base_port
            );
            return None;
        }
    };

    let invoker = match find_invoker(data_dir) {
        Some(i) => i,
        None => {
            println!("  📦 qwen-tts not found — installing automatically...");
            match ensure_installed(data_dir).await {
                Ok(i) => i,
                Err(e) => {
                    println!("  ⚠  Could not install qwen-tts: {}", e);
                    return None;
                }
            }
        }
    };

    println!("  🔊 Starting Qwen TTS ({})...", invoker.description());
    match spawn(invoker, port).await {
        Ok((proc, true)) => {
            println!("  ✅ Qwen TTS ready on port {}", port);
            Some((proc, port, true))
        }
        Ok((proc, false)) => {
            // Timed out but process is still alive — keep guard so it stays running.
            Some((proc, port, false))
        }
        Err(e) => {
            println!("  ⚠  Failed to start Qwen TTS: {}", e);
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
