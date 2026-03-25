//! Route definitions for GIAP REST API and web dashboard.
//!
//! # TODO
//! - [ ] Implement each handler with real logic
//! - [ ] Add request/response types in pond-core domain
//! - [ ] Serve static web dashboard files

use axum::{
    extract::{rejection::JsonRejection, Multipart, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use tower_http::services::ServeDir;
use pond_core::domain::onboarding::OnboardingStep;
use pond_core::ports::handshake::{HandshakeRequest, HandshakeResponse};
use pond_core::services::onboarding::OnboardingService;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;
use crate::middleware::onboarding_guard::require_onboarding_complete;

// ───────────────────────── REST API Routes ─────────────────────────

/// Builds the full REST API router with onboarding-aware middleware
pub fn api_routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    // ───────────── Public routes (accessible before onboarding) ─────────────
    let public_routes = Router::new()
        .route("/health", get(health))
        .route("/handshake", post(handshake_handler))
        .route("/onboard", post(start_onboarding))
        .route("/onboard/status", get(onboarding_status))
        // Transcription proxy (public — local test tool)
        .route("/transcribe", post(transcribe))
        .route("/system/info", get(system_info));

    // ───────────── Protected routes (require onboarding) ─────────────
    let protected_routes = Router::new()
        .route("/chat", post(chat))
        .route("/devices", get(list_devices).post(register_device))
        .route("/settings", get(get_settings).put(update_settings))
        .layer(
            axum::middleware::from_fn_with_state(state.clone(), require_onboarding_complete)
        );

    // Merge public and protected routes, attach shared state
    public_routes
        .merge(protected_routes)
        .with_state(state)
}

// ───────────────────────── Web Dashboard Routes ─────────────────────

/// Serves the built Vite assets from the given directory as a fallback service.
/// In development, use `npm run dev` instead (Vite dev server on port 5173).
pub fn web_routes(static_dir: std::path::PathBuf) -> ServeDir {
    ServeDir::new(static_dir)
}

// ───────────────────────── Handlers ─────────────────────────────────

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// Handshake endpoint to get authentication token (public)
///
/// TODO: Implement full GIAP ↔ GOTG handshake:
/// 1. Verify the GOTG client identity
/// 2. Exchange a session token
/// 3. Return connection details (hostname, port, capabilities)
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

/// Start or report onboarding state (public)
async fn start_onboarding(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let service = OnboardingService::new(state.onboarding_repo.clone());

    match service.status().await {
        Some(OnboardingStep::Completed) => Ok(Json(json!({
            "status": "already_complete",
            "message": "Onboarding has already been completed"
        }))),
        Some(step) => Ok(Json(json!({
            "status": "in_progress",
            "message": "Onboarding already started",
            "current_step": step.to_string()
        }))),
        None => {
            service.start().await.map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("Failed to start onboarding: {}", e)})),
                )
            })?;
            Ok(Json(json!({
                "status": "started",
                "current_step": OnboardingStep::VerifyDevice.to_string()
            })))
        }
    }
}

/// Return current onboarding progress (public)
async fn onboarding_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let service = OnboardingService::new(state.onboarding_repo.clone());

    let total_steps = 4;
    let (current_step, steps_completed, onboarded) = match service.status().await {
        None                                       => ("not_started".to_string(),                         0, false),
        Some(OnboardingStep::VerifyDevice)         => (OnboardingStep::VerifyDevice.to_string(),         1, false),
        Some(OnboardingStep::CreateProfile)        => (OnboardingStep::CreateProfile.to_string(),        2, false),
        Some(OnboardingStep::ConfigurePersonality) => (OnboardingStep::ConfigurePersonality.to_string(), 3, false),
        Some(OnboardingStep::ConnectDevices)       => (OnboardingStep::ConnectDevices.to_string(),       3, false),
        Some(OnboardingStep::Completed)            => ("Completed".to_string(),                          4, true),
    };

    Json(json!({
        "onboarded": onboarded,
        "current_step": current_step,
        "steps_completed": steps_completed,
        "total_steps": total_steps
    }))
}

/// TODO: Wire to ChatService + LlmProvider
async fn chat() -> Json<Value> {
    Json(json!({
        "status": "todo",
        "message": "Chat endpoint not yet wired to ChatService"
    }))
}

async fn system_info() -> Json<Value> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    Json(json!({
        "hostname": hostname,
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    }))
}

async fn list_devices(State(_state): State<Arc<AppState>>) -> Json<Value> {
    // TODO: Query system DB for registered devices
    Json(json!({ "devices": [] }))
}

async fn register_device(State(_state): State<Arc<AppState>>) -> Json<Value> {
    // TODO: Insert device into system DB
    Json(json!({ "status": "todo" }))
}

async fn get_settings(State(_state): State<Arc<AppState>>) -> Json<Value> {
    // TODO: Query system DB for settings
    Json(json!({ "settings": {} }))
}

async fn update_settings(State(_state): State<Arc<AppState>>) -> Json<Value> {
    // TODO: Update settings in system DB
    Json(json!({ "status": "todo" }))
}

/// Proxy multipart audio to the whisper.cpp server and return the transcript.
///
/// This route is intentionally public (no auth required). It is a local
/// development / testing tool and is only expected to be reachable from
/// localhost. Do not expose the GIAP server to the internet without adding
/// authentication to this endpoint.
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

    let resp = state
        .http_client
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
