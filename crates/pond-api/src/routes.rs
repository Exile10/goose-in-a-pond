//! Route definitions for GIAP REST API and web dashboard.
//!
//! # Authentication
//! Protected routes require a bearer token. Call POST /api/v1/handshake first to get a token.
//!
//! # TODO
//! - [ ] Implement each handler with real logic
//! - [ ] Add request/response types in pond-core domain
//! - [ ] Serve static web dashboard files

use axum::{
    extract::{rejection::JsonRejection, Multipart, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use pond_core::ports::handshake::{HandshakeRequest, HandshakeResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::{middleware, AppState};

// ───────────────────────── REST API Routes ─────────────────────────

pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        // Health (public)
        .route("/health", get(health))
        // Onboarding (public)
        .route("/handshake", post(handshake_handler))
        .route("/onboard", post(start_onboarding))
        .route("/onboard/status", get(onboarding_status))
        // Transcription proxy (public — local test tool)
        .route("/transcribe", post(transcribe))
        // Chat (protected)
        .route("/chat", post(chat))
        // System (protected)
        .route("/system/info", get(system_info))
        // Devices (protected)
        .route("/devices", get(list_devices).post(register_device))
        // Settings (protected)
        .route("/settings", get(get_settings).put(update_settings))
}

// ───────────────────────── Web Dashboard Routes ─────────────────────

pub fn web_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(dashboard_index))
        // TODO: Serve static files for the web dashboard
        // .nest_service("/assets", ServeDir::new("static"))
}

// ───────────────────────── Helper Functions ─────────────────────────

/// Validate a token and return 401 if invalid
async fn validate_token(
    headers: &HeaderMap,
    state: &Arc<AppState>,
) -> Result<String, (axum::http::StatusCode, Json<Value>)> {
    let token = middleware::extract_bearer_token(headers).map_err(|e| {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": format!("{:?}", e),
                "status": 401
            })),
        )
    })?;

    let is_valid = state
        .handshake
        .validate_token(&token)
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": format!("Token validation failed: {}", e),
                    "status": 500
                })),
            )
        })?;

    if !is_valid {
        return Err((
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "Invalid or expired token",
                "status": 401
            })),
        ));
    }

    Ok(token)
}

// ───────────────────────── Handlers ─────────────────────────────────

/// Health check endpoint (public)
async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Handshake endpoint to get authentication token (public)
async fn handshake_handler(
    State(state): State<Arc<AppState>>,
    body: Result<Json<HandshakeRequest>, JsonRejection>,
) -> Result<Json<HandshakeResponse>, (axum::http::StatusCode, Json<Value>)> {
    let Json(request) = body.map_err(|e| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("Invalid request: {}", e),
                "status": 400
            })),
        )
    })?;

    let response = state
        .handshake
        .handshake(request)
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": format!("Handshake failed: {}", e),
                    "status": 500
                })),
            )
        })?;

    Ok(Json(response))
}

/// Start onboarding flow (public)
async fn start_onboarding() -> Json<Value> {
    // TODO: Implement onboarding flow
    Json(json!({
        "status": "todo",
        "message": "Onboarding not yet implemented"
    }))
}

/// Get onboarding status (public)
async fn onboarding_status() -> Json<Value> {
    // TODO: Return onboarding progress
    Json(json!({
        "status": "todo",
        "onboarded": false,
        "steps_completed": 0,
        "total_steps": 4
    }))
}

/// Send a message to the chat (protected)
async fn chat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    // Validate token
    let _token = validate_token(&headers, &state).await?;

    // TODO: Wire to ChatService + LlmProvider
    Ok(Json(json!({
        "status": "todo",
        "message": "Chat endpoint not yet wired to ChatService"
    })))
}

/// Get system information (protected)
async fn system_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    // Validate token
    let _token = validate_token(&headers, &state).await?;

    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    Ok(Json(json!({
        "hostname": hostname,
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    })))
}

/// List registered devices (protected)
async fn list_devices(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    // Validate token
    let _token = validate_token(&headers, &state).await?;

    // TODO: Query system DB for registered devices
    Ok(Json(json!({ "devices": [] })))
}

/// Register a new device (protected)
async fn register_device(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    // Validate token
    let _token = validate_token(&headers, &state).await?;

    // TODO: Insert device into system DB
    Ok(Json(json!({ "status": "todo" })))
}

/// Get current settings (protected)
async fn get_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    // Validate token
    let _token = validate_token(&headers, &state).await?;

    // TODO: Query system DB for settings
    Ok(Json(json!({ "settings": {} })))
}

/// Update settings (protected)
async fn update_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    // Validate token
    let _token = validate_token(&headers, &state).await?;

    // TODO: Update settings in system DB
    Ok(Json(json!({ "status": "todo" })))
}

/// Web dashboard — transcription test UI
async fn dashboard_index() -> axum::response::Html<&'static str> {
    axum::response::Html(TRANSCRIBE_HTML)
}

/// Proxy multipart audio to the whisper.cpp server and return the transcript.
async fn transcribe(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Read the "audio" field from the multipart body
    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut filename = "audio.bin".to_string();
    let mut content_type = "audio/wav".to_string();

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("multipart error: {}", e)})),
        )
    })? {
        if field.name() == Some("audio") {
            filename = field
                .file_name()
                .unwrap_or("audio.bin")
                .to_string();
            content_type = field
                .content_type()
                .unwrap_or("audio/wav")
                .to_string();
            let bytes = field.bytes().await.map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("read error: {}", e)})),
                )
            })?;
            audio_bytes = Some(bytes.to_vec());
        }
    }

    let bytes = audio_bytes.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing 'audio' field in multipart body"})),
        )
    })?;

    // Forward to whisper.cpp /inference
    let whisper_url = format!("{}/inference", state.whisper_url);
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str(&content_type)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("MIME error: {}", e)})),
            )
        })?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json");

    let client = reqwest::Client::new();
    let resp = client
        .post(&whisper_url)
        .multipart(form)
        .send()
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("whisper server unreachable: {}", e)})),
            )
        })?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": body})),
        ));
    }

    let json: Value = resp.json().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("whisper response parse error: {}", e)})),
        )
    })?;

    let text = json["text"].as_str().unwrap_or("").trim().to_string();
    Ok(Json(json!({"text": text})))
}

// ─────────────────────────────────────────────────────────────────────────────
// Transcribe UI — self-contained HTML (no external CSS/JS dependencies)
// ─────────────────────────────────────────────────────────────────────────────

const TRANSCRIBE_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Goose in a Pond — Transcribe</title>
<style>
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:system-ui,sans-serif;background:#111827;color:#e5e7eb;
  min-height:100vh;display:flex;align-items:center;justify-content:center;padding:1rem}
.card{background:#1f2937;border:1px solid #374151;border-radius:12px;
  padding:2rem;width:100%;max-width:580px}
h1{font-size:1.3rem;margin-bottom:1.5rem;color:#f3f4f6}
.row{display:flex;gap:.5rem;margin-bottom:1rem}
input[type=file]{flex:1;background:#111827;border:1px solid #374151;border-radius:6px;
  color:#d1d5db;padding:.4rem .6rem;font-size:.9rem}
button{background:#1d4ed8;color:#fff;border:none;border-radius:6px;
  padding:.5rem 1.1rem;cursor:pointer;font-size:.9rem;white-space:nowrap}
button:hover{background:#2563eb}
button:disabled{opacity:.5;cursor:not-allowed}
.divider{text-align:center;color:#6b7280;margin:1.2rem 0;font-size:.85rem}
#micBtn{width:100%;padding:.75rem;font-size:1rem}
#micBtn.recording{background:#991b1b;animation:pulse 1s infinite}
@keyframes pulse{0%,100%{opacity:1}50%{opacity:.65}}
#status{margin-top:.6rem;font-size:.85rem;color:#9ca3af;min-height:1.2rem}
#result{margin-top:1rem;background:#111827;border:1px solid #374151;border-radius:6px;
  padding:1rem;min-height:80px;font-size:.95rem;line-height:1.6;
  color:#e5e7eb;white-space:pre-wrap}
.err{color:#f87171}
</style>
</head>
<body>
<div class="card">
  <h1>🎙 Goose in a Pond &mdash; Transcribe</h1>

  <div class="row">
    <input type="file" id="fileInput" accept="audio/*">
    <button id="uploadBtn" onclick="transcribeFile()">Transcribe</button>
  </div>

  <div class="divider">── or ──</div>

  <button id="micBtn" onclick="toggleMic()">🎤 Start Recording</button>

  <div id="status"></div>
  <div id="result">Transcript will appear here…</div>
</div>

<script>
var $status   = document.getElementById('status');
var $result   = document.getElementById('result');
var $micBtn   = document.getElementById('micBtn');
var $uploadBtn= document.getElementById('uploadBtn');
var $fileInput= document.getElementById('fileInput');

// ── File upload ───────────────────────────────────────────────────────────────
async function transcribeFile() {
  var file = $fileInput.files[0];
  if (!file) { setStatus('Choose an audio file first.'); return; }
  setStatus('Uploading\u2026');
  $uploadBtn.disabled = true;
  try {
    var fd = new FormData();
    fd.append('audio', file, file.name);
    showResult(await postAudio(fd));
  } catch(e) { showErr(e.message); }
  finally { $uploadBtn.disabled = false; setStatus(''); }
}

// ── Microphone recording (Web Audio API → WAV) ────────────────────────────────
var audioCtx = null, processor = null, micStream = null;
var pcmBufs = [];

async function toggleMic() {
  if (audioCtx) {
    // Stop recording
    processor.disconnect();
    audioCtx.close();
    micStream.getTracks().forEach(function(t){ t.stop(); });
    var totalLen = pcmBufs.reduce(function(a,b){ return a+b.length; }, 0);
    var samples = new Float32Array(totalLen);
    var off = 0;
    for (var i = 0; i < pcmBufs.length; i++) {
      samples.set(pcmBufs[i], off);
      off += pcmBufs[i].length;
    }
    processor = null; audioCtx = null; micStream = null; pcmBufs = [];
    $micBtn.textContent = '\uD83C\uDF99\uFE0F Start Recording';
    $micBtn.classList.remove('recording');
    $micBtn.disabled = true;
    setStatus('Transcribing\u2026');
    try {
      var wav = encodeWav(samples, 16000);
      var fd = new FormData();
      fd.append('audio', wav, 'recording.wav');
      showResult(await postAudio(fd));
    } catch(e) { showErr(e.message); }
    finally { $micBtn.disabled = false; setStatus(''); }
    return;
  }
  // Start recording
  try {
    micStream = await navigator.mediaDevices.getUserMedia({ audio: true });
  } catch(e) { setStatus('Mic denied: ' + e.message); return; }
  pcmBufs = [];
  audioCtx = new AudioContext({ sampleRate: 16000 });
  var src = audioCtx.createMediaStreamSource(micStream);
  processor = audioCtx.createScriptProcessor(4096, 1, 1);
  processor.onaudioprocess = function(e) {
    var data = e.inputBuffer.getChannelData(0);
    pcmBufs.push(new Float32Array(data));
  };
  src.connect(processor);
  processor.connect(audioCtx.destination);
  $micBtn.textContent = '\u23F9 Stop Recording';
  $micBtn.classList.add('recording');
  setStatus('Recording\u2026 click Stop when done.');
}

// ── WAV encoder (matches Rust encode_wav_mono_16k) ────────────────────────────
function encodeWav(samples, sr) {
  var ch = 1, bps = 16;
  var dataLen = samples.length * 2;
  var buf = new ArrayBuffer(44 + dataLen);
  var v = new DataView(buf);
  function ws(o, t) { for (var i=0;i<t.length;i++) v.setUint8(o+i, t.charCodeAt(i)); }
  ws(0,'RIFF'); v.setUint32(4, 36+dataLen, true); ws(8,'WAVE');
  ws(12,'fmt '); v.setUint32(16,16,true); v.setUint16(20,1,true); v.setUint16(22,ch,true);
  v.setUint32(24,sr,true); v.setUint32(28,sr*ch*bps/8,true);
  v.setUint16(32,ch*bps/8,true); v.setUint16(34,bps,true);
  ws(36,'data'); v.setUint32(40,dataLen,true);
  var o = 44;
  for (var i = 0; i < samples.length; i++) {
    var c = Math.max(-1, Math.min(1, samples[i]));
    v.setInt16(o, c < 0 ? c * 0x8000 : c * 0x7FFF, true);
    o += 2;
  }
  return new Blob([buf], { type: 'audio/wav' });
}

// ── Helpers ───────────────────────────────────────────────────────────────────
async function postAudio(fd) {
  var resp = await fetch('/api/v1/transcribe', { method: 'POST', body: fd });
  var j = await resp.json();
  if (!resp.ok || j.error) throw new Error(j.error || 'Server error');
  return j.text || '(empty transcript)';
}
function setStatus(msg)  { $status.textContent = msg; }
function showResult(txt) { $result.textContent = txt; $result.className = ''; }
function showErr(msg)    { $result.textContent = 'Error: ' + msg; $result.className = 'err'; }
</script>
</body>
</html>"##;
