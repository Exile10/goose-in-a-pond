//! Lightweight HTTP server that wraps the Piper TTS subprocess.
//!
//! Spawns as a background Tokio task on `127.0.0.1:<port>`.
//! Endpoint: `POST /tts`  — plain-text body → WAV audio bytes.
//!
//! This gives Piper the same "running on port N" lifecycle as
//! whisper-server so it can be reported in the startup banner
//! and reached by any local HTTP client.

use anyhow::{anyhow, Context, Result};
use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::Response,
    routing::post,
    Router,
};
use std::{io::Write as _, path::PathBuf, sync::Arc};

// Port is set in crate::ports::PIPER_TTS — no constant here.

// ── Server state ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct PiperState {
    piper_bin: PathBuf,
    model: PathBuf,
    espeak_data: Option<PathBuf>,
}

// ── Handler ───────────────────────────────────────────────────────────────────

async fn synthesise(
    State(s): State<Arc<PiperState>>,
    body: String,
) -> Result<Response<Body>, StatusCode> {
    let bin = s.piper_bin.clone();
    let model = s.model.clone();
    let espeak_data = s.espeak_data.clone();
    let text = body.trim().to_string();

    if text.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let wav =
        tokio::task::spawn_blocking(move || run_piper(&bin, &model, espeak_data.as_deref(), &text))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_err(|e| {
                tracing::warn!("piper synthesis failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::CONTENT_LENGTH, wav.len())
        .body(Body::from(wav))
        .unwrap())
}

fn run_piper(
    piper_bin: &std::path::Path,
    model: &std::path::Path,
    espeak_data: Option<&std::path::Path>,
    text: &str,
) -> Result<Vec<u8>> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(piper_bin);
    cmd.args(["--model", &model.to_string_lossy()])
        .args(["--output-raw", "--quiet"]);
    if let Some(d) = espeak_data {
        cmd.args(["--espeak_data", &d.to_string_lossy()]);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn piper at {}", piper_bin.display()))?;

    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("piper stdin unavailable"))?
        .write_all(text.as_bytes())
        .context("write to piper stdin")?;

    let out = child.wait_with_output().context("waiting for piper")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            return Err(anyhow!("piper exited {}", out.status));
        }
        return Err(anyhow!("piper exited {}: {}", out.status, stderr));
    }

    Ok(pcm_to_wav(&out.stdout, 22_050))
}

fn pcm_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let channels: u16 = 1;
    let bits: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits) / 8;
    let block_align: u16 = channels * bits / 8;
    let data_len = pcm.len() as u32;

    let mut w = Vec::with_capacity(44 + pcm.len());
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVE");
    w.extend_from_slice(b"fmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&channels.to_le_bytes());
    w.extend_from_slice(&sample_rate.to_le_bytes());
    w.extend_from_slice(&byte_rate.to_le_bytes());
    w.extend_from_slice(&block_align.to_le_bytes());
    w.extend_from_slice(&bits.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    w.extend_from_slice(pcm);
    w
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Start the Piper HTTP server in a background task.
///
/// The base port is taken from [`crate::ports::PIPER_TTS`].  If that port is
/// busy, the next port in arithmetic sequence is tried automatically.
/// Returns the port actually bound, or `Err` if no port could be secured.
pub async fn start(
    piper_bin: PathBuf,
    model: PathBuf,
    espeak_data: Option<PathBuf>,
) -> Result<u16> {
    let (listener, actual_port) =
        crate::ports::bind_with_fallback("127.0.0.1", crate::ports::PIPER_TTS)
            .await
            .with_context(|| {
                format!(
                    "piper-http: could not bind port {}",
                    crate::ports::PIPER_TTS
                )
            })?;

    let state = Arc::new(PiperState {
        piper_bin,
        model,
        espeak_data,
    });
    let app = Router::new()
        .route("/tts", post(synthesise))
        .with_state(state);

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!("piper-http server stopped: {e}");
        }
    });

    Ok(actual_port)
}

/// Returns `true` if a piper-http server is already responding on `port`.
#[allow(dead_code)]
pub async fn is_running(port: u16) -> bool {
    reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/tts"))
        .body("ping")
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}
