//! Route definitions for GIAP REST API.
//!
//! # TODO
//! - [ ] Implement each handler with real logic
//! - [ ] Add authentication/handshake middleware
//! - [ ] Add request/response types in pond-core domain

use axum::{
    extract::State,
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower_http::services::ServeDir;

use crate::AppState;

// ───────────────────────── Web Dashboard Routes ─────────────────────────

/// Serves the built Vite assets from the given directory.
/// In development, use `npm run dev` instead (Vite dev server on port 5173).
pub fn web_routes(static_dir: std::path::PathBuf) -> tower_http::services::ServeDir {
    ServeDir::new(static_dir)
}

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

/// TODO: Start the onboarding process
/// 1. Create initial user profile
/// 2. Configure device settings
/// 3. Set up system prompt / personality
/// 4. Register initial smart devices
async fn start_onboarding() -> Json<Value> {
    // TODO: Implement onboarding flow
    Json(json!({
        "status": "todo",
        "message": "Onboarding not yet implemented"
    }))
}

async fn onboarding_status() -> Json<Value> {
    // TODO: Return onboarding progress
    Json(json!({
        "status": "todo",
        "onboarded": false,
        "steps_completed": 0,
        "total_steps": 4
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

