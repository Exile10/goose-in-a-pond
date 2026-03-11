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
    extract::{rejection::JsonRejection, State},
    http::HeaderMap,
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

/// Web dashboard index
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
