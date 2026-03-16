//! Route definitions for GIAP REST API and web dashboard.
//!
//! # TODO
//! - [ ] Implement each handler with real logic
//! - [ ] Add authentication/handshake middleware
//! - [ ] Add request/response types in pond-core domain
//! - [ ] Serve static web dashboard files

use axum::{
    extract::State,
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;
use pond_core::domain::onboarding::OnboardingStep;
use pond_core::services::onboarding::OnboardingService;
use crate::AppState;

// ───────────────────────── REST API Routes ─────────────────────────

pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        // Health
        .route("/health", get(health))
        // Onboarding
        .route("/handshake", post(handshake))
        .route("/onboard", post(start_onboarding))
        .route("/onboard/status", get(onboarding_status))
        // Chat
        .route("/chat", post(chat))
        // System
        .route("/system/info", get(system_info))
        // Devices
        .route("/devices", get(list_devices).post(register_device))
        // Settings
        .route("/settings", get(get_settings).put(update_settings))
}

// ───────────────────────── Web Dashboard Routes ─────────────────────

pub fn web_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(dashboard_index))
        // TODO: Serve static files for the web dashboard
        // .nest_service("/assets", ServeDir::new("static"))
}

// ───────────────────────── Handlers ─────────────────────────────────

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// TODO: Implement GIAP ↔ GOTG handshake protocol
/// The handshake should:
/// 1. Verify the GOTG client identity
/// 2. Exchange a session token
/// 3. Return connection details (hostname, port, capabilities)
async fn handshake() -> Json<Value> {
    // TODO: Implement handshake logic
    Json(json!({
        "status": "todo",
        "message": "Handshake not yet implemented. See TODO in routes.rs"
    }))
}

async fn start_onboarding(State(state): State<Arc<AppState>>) -> Json<Value> {
    let service = OnboardingService::new(state.onboarding_repo.clone());

    match service.status().await {
        Some(OnboardingStep::Completed) => Json(json!({
            "status": "already_complete",
            "message": "Onboarding has already been completed"
        })),
        Some(step) => Json(json!({
            "status": "in_progress",
            "message": "Onboarding already started",
            "current_step": step.to_string()
        })),
        None => {
            service.start().await;
            Json(json!({
                "status": "started",
                "current_step": OnboardingStep::VerifyDevice.to_string()
            }))
        }
    }
}

async fn onboarding_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let service = OnboardingService::new(state.onboarding_repo.clone());

    let total_steps = 4;
    let (current_step, steps_completed, onboarded) = match service.status().await {
        None                                      => ("not_started".to_string(),                         0, false),
        Some(OnboardingStep::VerifyDevice)        => (OnboardingStep::VerifyDevice.to_string(),        1, false),
        Some(OnboardingStep::CreateProfile)       => (OnboardingStep::CreateProfile.to_string(),       2, false),
        Some(OnboardingStep::ConfigurePersonality)=> (OnboardingStep::ConfigurePersonality.to_string(),3, false),
        Some(OnboardingStep::ConnectDevices)      => (OnboardingStep::ConnectDevices.to_string(),      4, false),
        Some(OnboardingStep::Completed)           => ("Completed".to_string(),                         4, true),
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

async fn dashboard_index() -> axum::response::Html<&'static str> {
    // TODO: Replace with a real web dashboard (SPA or server-rendered)
    axum::response::Html(
        r#"<!DOCTYPE html>
<html>
<head><title>Goose In A Pond</title></head>
<body>
  <h1>🦆 Goose In A Pond</h1>
  <p>Dashboard coming soon.</p>
  <p><a href="/api/v1/health">API Health Check</a></p>
</body>
</html>"#,
    )
}
