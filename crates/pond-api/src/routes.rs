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
    middleware,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;
use crate::middleware ::onboarding_guard::require_onboarding_complete;


// ───────────────────────── REST API Routes ─────────────────────────

/// Builds the full REST API router with onboarding-aware middleware
pub fn api_routes(state: Arc<AppState>) -> Router<Arc<AppState>> {
    // ───────────── Public routes (accessible before onboarding) ─────────────
    let public_routes = Router::new()
        .route("/health", get(health))
        .route("/handshake", post(handshake))
        .route("/onboard", post(start_onboarding))
        .route("/onboard/status", get(onboarding_status))
        .route("/system/info", get(system_info));

    // ───────────── Protected routes (require onboarding) ─────────────
    let protected_routes = Router::new()
        .route("/chat", post(chat))
        .route("/devices", get(list_devices).post(register_device))
        .route("/settings", get(get_settings).put(update_settings))
        .layer(
            // Apply middleware to ensure onboarding is complete
            middleware::from_fn_with_state(state.clone(), require_onboarding_complete)
        );

    // Merge public and protected routes, attach shared state
    public_routes
        .merge(protected_routes)
        .with_state(state)
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
