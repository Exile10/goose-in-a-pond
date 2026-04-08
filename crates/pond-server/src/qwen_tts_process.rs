//! Auto-start helper for the Qwen3-TTS HTTP server.
//!
//! `qwen-tts` (v0.1.1+) is Qwen3-TTS, a pure-Python library with no built-in
//! `serve` command.  We embed a minimal OpenAI-compatible HTTP server script
//! (`SERVER_SCRIPT`) and write it to `<data_dir>/qwen_tts_serve.py` at
//! startup.  The script:
//!
//!   - Auto-detects the best device: CUDA → MPS (Apple Silicon) → CPU
//!   - Loads `Qwen3TTSModel` from HuggingFace (cached after first run)
//!   - Exposes `GET /` (health) and `POST /v1/audio/speech` → WAV bytes
//!
//! Install + start order on first run:
//!   1. Check if the managed venv exists and has `qwen_tts` installed.
//!   2. If not: find Python 3.10+, create venv, `pip install qwen-tts`.
//!   3. Write `qwen_tts_serve.py` to `data_dir` (always refreshed).
//!   4. Spawn `<venv>/python qwen_tts_serve.py --port <port>`.
//!   5. Poll until the server responds or 120 s elapses.
//!   6. Return a guard that kills the process on drop.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Child;

// ── Embedded server script ────────────────────────────────────────────────────

/// The Python HTTP server we write to disk and spawn.
///
/// Design: the HTTP server binds immediately so the health-check poll
/// succeeds within a second, while the model loads in a background thread.
/// Speech requests that arrive before the model is ready return 503 so the
/// Rust adapter can retry rather than hanging.
const SERVER_SCRIPT: &str = r#"#!/usr/bin/env python3
"""Minimal OpenAI-compatible TTS server backed by qwen-tts (Qwen3-TTS).

Endpoints:
  GET  /                 -> 200 OK | 503 Loading...
  POST /v1/audio/speech  body: {"input": "...", "voice": "Vivian"}
                         -> 200 audio/wav  |  503 (model still loading)
"""

import io
import json
import threading
import wave
import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer

import numpy as np


# ── Device auto-detection ─────────────────────────────────────────────────────

def best_device():
    """Return (device_str, dtype_str) best suited to this machine."""
    import torch
    if torch.cuda.is_available():
        return "cuda", "bfloat16"
    try:
        if torch.backends.mps.is_available():
            return "mps", "float32"   # bfloat16 not fully supported on MPS
    except AttributeError:
        pass
    return "cpu", "float32"


# ── Model state ───────────────────────────────────────────────────────────────

_tts        = None          # set once loaded
_loading    = True          # False when done (success or failure)
_load_error = None          # str if loading failed


def _load_background(checkpoint: str, device: str, dtype_str: str):
    global _tts, _loading, _load_error
    try:
        import torch
        from qwen_tts import Qwen3TTSModel

        if device == "auto":
            device, dtype_str = best_device()

        dtype_map = {
            "bfloat16": torch.bfloat16,
            "float32":  torch.float32,
            "float16":  torch.float16,
        }
        dtype = dtype_map.get(dtype_str, torch.float32)

        print(f"[qwen-tts] Loading {checkpoint} on {device} ({dtype_str})...", flush=True)
        _tts = Qwen3TTSModel.from_pretrained(
            checkpoint,
            device_map=device,
            dtype=dtype,
            attn_implementation=None,
        )
        print("[qwen-tts] Model ready.", flush=True)
    except Exception as exc:
        _load_error = str(exc)
        print(f"[qwen-tts] Model load failed: {exc}", flush=True)
    finally:
        _loading = False


# ── Audio helpers ─────────────────────────────────────────────────────────────

def pcm_to_wav(pcm, sr: int) -> bytes:
    pcm16 = (np.clip(np.asarray(pcm, dtype=np.float32), -1.0, 1.0) * 32767).astype(np.int16)
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(sr)
        wf.writeframes(pcm16.tobytes())
    return buf.getvalue()


# ── HTTP handler ──────────────────────────────────────────────────────────────

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if _loading:
            self._respond(503, b"Loading model...")
        elif _load_error:
            self._respond(503, f"Load failed: {_load_error}".encode())
        else:
            self._respond(200, b"OK")

    def do_POST(self):
        if self.path != "/v1/audio/speech":
            self._respond(404, b"Not found")
            return

        if _loading:
            self._respond(503, b"Model still loading, try again shortly")
            return
        if _load_error:
            self._respond(503, f"Load failed: {_load_error}".encode())
            return

        length = int(self.headers.get("Content-Length", 0))
        try:
            body = json.loads(self.rfile.read(length))
        except Exception:
            self._respond(400, b"invalid JSON")
            return

        text  = (body.get("input") or "").strip()
        voice = body.get("voice") or "Vivian"

        if not text:
            self._respond(400, b'"input" is required')
            return

        try:
            wavs, sr = _tts.generate_custom_voice(
                text=text,
                language="Auto",
                speaker=voice,
            )
            audio = pcm_to_wav(wavs[0], sr)
            self.send_response(200)
            self.send_header("Content-Type", "audio/wav")
            self.send_header("Content-Length", str(len(audio)))
            self.end_headers()
            self.wfile.write(audio)
        except Exception as exc:
            self._respond(500, str(exc).encode())

    def _respond(self, code: int, body: bytes):
        self.send_response(code)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        pass  # suppress per-request access logs


# ── Entry point ───────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Qwen3-TTS OpenAI-compatible HTTP server")
    parser.add_argument("--port",       type=int, default=8181,
                        help="Port to listen on (default: 8181)")
    parser.add_argument("--checkpoint", default="Qwen/Qwen3-TTS-12Hz-1.7B-CustomVoice",
                        help="HuggingFace repo id or local path")
    parser.add_argument("--device",     default="auto",
                        help="Torch device: auto | cpu | cuda | mps")
    parser.add_argument("--dtype",      default="auto",
                        help="Torch dtype: auto | float32 | bfloat16 | float16")
    args = parser.parse_args()

    dtype = args.dtype if args.dtype != "auto" else "float32"

    # Bind the server first so health checks succeed immediately.
    server = HTTPServer(("127.0.0.1", args.port), Handler)
    print(f"[qwen-tts] Listening on 127.0.0.1:{args.port}", flush=True)

    # Load model in background so startup is non-blocking.
    t = threading.Thread(target=_load_background, args=(args.checkpoint, args.device, dtype), daemon=True)
    t.start()

    server.serve_forever()


if __name__ == "__main__":
    main()
"#;

// ── Guard ─────────────────────────────────────────────────────────────────────

/// Holds the spawned server child process.  Kills it on drop.
pub struct QwenTtsProcess(Child);

impl Drop for QwenTtsProcess {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
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

fn venv_site_packages(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    return venv_dir(data_dir).join("Lib").join("site-packages");

    #[cfg(not(windows))]
    {
        let lib = venv_dir(data_dir).join("lib");
        if let Ok(entries) = std::fs::read_dir(&lib) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("python") {
                    return entry.path().join("site-packages");
                }
            }
        }
        lib.join("site-packages")
    }
}

fn venv_has_qwen_tts(data_dir: &Path) -> bool {
    venv_site_packages(data_dir).join("qwen_tts").exists()
}

fn server_script_path(data_dir: &Path) -> PathBuf {
    data_dir.join("qwen_tts_serve.py")
}

// ── Script management ─────────────────────────────────────────────────────────

/// Write (or overwrite) the embedded server script to disk.
fn write_server_script(data_dir: &Path) -> Result<PathBuf> {
    let path = server_script_path(data_dir);
    std::fs::write(&path, SERVER_SCRIPT)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}

// ── Python discovery ──────────────────────────────────────────────────────────

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

/// Find Python 3.10+ on PATH.
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
        let needs_check = !name.contains("3.10")
            && !name.contains("3.11")
            && !name.contains("3.12")
            && !name.contains("3.13");
        if needs_check {
            let ok = std::process::Command::new(&path)
                .args(["-c", "import sys; v=sys.version_info; exit(0 if (v.major,v.minor)>=(3,10) else 1)"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok { continue; }
        }
        return Some(path);
    }
    None
}

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
        for candidate in &["/opt/homebrew/bin/python3.12", "/usr/local/bin/python3.12"] {
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

    find_python()
}

// ── Venv version check ────────────────────────────────────────────────────────

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

// ── Install ───────────────────────────────────────────────────────────────────

/// Ensure qwen-tts is installed in the managed venv and write the server script.
async fn ensure_installed(data_dir: &Path) -> Result<()> {
    let venv = venv_dir(data_dir);

    let python = find_or_install_python()
        .await
        .ok_or_else(|| anyhow::anyhow!("Python 3.10+ unavailable and auto-install failed"))?;

    // Recreate venv if it was built with Python < 3.10.
    if venv.exists() {
        if let Some((maj, min)) = venv_python_version(data_dir).await {
            if maj < 3 || (maj == 3 && min < 10) {
                println!("  🔄 Recreating venv (Python {}.{} < 3.10)...", maj, min);
                tokio::fs::remove_dir_all(&venv).await.ok();
            }
        }
    }

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

    // Upgrade pip first.
    let _ = tokio::process::Command::new(&pip)
        .args(["install", "--upgrade", "pip"])
        .status()
        .await;

    // numpy must come before qwen-tts (its setup.py may import it).
    let _ = tokio::process::Command::new(&pip)
        .args(["install", "numpy"])
        .status()
        .await;

    println!("  📦 Installing qwen-tts...");
    let status = tokio::process::Command::new(&pip)
        .args(["install", "qwen-tts"])
        .status()
        .await
        .context("Failed to run pip install")?;
    if !status.success() {
        anyhow::bail!("`pip install qwen-tts` failed (exit {})", status);
    }

    write_server_script(data_dir)?;
    Ok(())
}

// ── Health check ──────────────────────────────────────────────────────────────

/// Returns `true` if the server process is up (any HTTP response).
pub async fn is_running(base_url: &str) -> bool {
    reqwest::Client::new()
        .get(base_url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

/// Returns `true` only when the model has finished loading (200 OK, body "OK").
/// A 503 means the server is up but still loading — not an error.
async fn is_model_ready(base_url: &str) -> bool {
    let resp = reqwest::Client::new()
        .get(base_url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().as_u16() == 200 => true,
        _ => false,
    }
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Default HuggingFace checkpoint used when no specific one is configured.
pub const DEFAULT_QWEN_CHECKPOINT: &str = "Qwen/Qwen3-TTS-12Hz-1.7B-CustomVoice";

/// Spawn the server script and wait for the HTTP server to bind (≤10 s).
///
/// `checkpoint` — HuggingFace repo id or local path for the Qwen TTS model.
/// `confirmed = true` means the HTTP server is accepting connections.
/// The model may still be loading in the background — speech requests
/// return 503 until it's ready, which `FallbackVoiceOutput` handles
/// transparently by routing to Piper.
async fn spawn(python: &Path, script: &Path, port: u16, checkpoint: &str) -> Result<(QwenTtsProcess, bool)> {
    let child = tokio::process::Command::new(python)
        .arg(script)
        .args(["--port", &port.to_string()])
        .args(["--checkpoint", checkpoint])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn {} {}", python.display(), script.display()))?;

    let proc = QwenTtsProcess(child);
    let url = format!("http://127.0.0.1:{}", port);

    // Wait for the HTTP server to bind (the script does this before loading the model).
    for _ in 1..=10 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if is_running(&url).await {
            // Check if model is already loaded (cache hit on a fast machine).
            if is_model_ready(&url).await {
                println!("  ✅ Qwen TTS ready on port {}", port);
            } else {
                println!("  ✅ Qwen TTS server bound on port {} — model loading in background (Piper handles TTS until ready)", port);
            }
            return Ok((proc, true));
        }
    }

    println!("  ⚠  Qwen TTS process did not bind within 10 s.");
    Ok((proc, false))
}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

/// Ensure Qwen TTS is installed, then start it if not already running.
///
/// `checkpoint` — HuggingFace repo id or local path to use.  Pass
/// `settings.voice_tts_http_voice` when non-empty, or [`DEFAULT_QWEN_CHECKPOINT`]
/// as the fallback so the model is always explicit rather than relying on the
/// Python script's hardcoded argparse default.
pub async fn try_start(data_dir: &Path, checkpoint: &str) -> Option<(QwenTtsProcess, u16, bool)> {
    let base_port = crate::ports::QWEN_TTS;
    let base_url = url_for(base_port);

    // Use the caller-supplied checkpoint, or the known default if empty.
    let effective_checkpoint = if checkpoint.is_empty() {
        DEFAULT_QWEN_CHECKPOINT
    } else {
        checkpoint
    };

    if is_running(&base_url).await {
        println!("  🔊 Qwen TTS already running at {}", base_url);
        return None;
    }

    let port = match crate::ports::find_free_port(base_port).await {
        Some(p) => p,
        None => {
            println!("  ⚠  No free port found near {} for Qwen TTS", base_port);
            return None;
        }
    };

    // Ensure venv + package + script are all present.
    let python = venv_python(data_dir);
    if !python.exists() || !venv_has_qwen_tts(data_dir) {
        println!("  📦 qwen-tts not found — installing automatically...");
        if let Err(e) = ensure_installed(data_dir).await {
            println!("  ⚠  Could not install qwen-tts: {}", e);
            return None;
        }
    }

    // Always refresh the script so updates take effect after a binary upgrade.
    if let Err(e) = write_server_script(data_dir) {
        println!("  ⚠  Could not write qwen_tts_serve.py: {}", e);
        return None;
    }

    let script = server_script_path(data_dir);
    println!("  🔊 Starting Qwen TTS server (checkpoint: {})...", effective_checkpoint);
    match spawn(&python, &script, port, effective_checkpoint).await {
        Ok((proc, true)) => {
            println!("  ✅ Qwen TTS ready on port {}", port);
            Some((proc, port, true))
        }
        Ok((proc, false)) => Some((proc, port, false)),
        Err(e) => {
            println!("  ⚠  Failed to start Qwen TTS: {}", e);
            None
        }
    }
}

/// Install Qwen TTS during `pond-server setup`. Returns `true` if ready.
pub async fn setup_install(data_dir: &Path) -> bool {
    if venv_python(data_dir).exists() && venv_has_qwen_tts(data_dir) {
        if let Ok(_) = write_server_script(data_dir) {
            println!("  ✅ Qwen TTS already installed.");
            return true;
        }
    }
    match ensure_installed(data_dir).await {
        Ok(_)  => true,
        Err(e) => {
            println!("  ⚠  Qwen TTS auto-install failed: {}", e);
            false
        }
    }
}
