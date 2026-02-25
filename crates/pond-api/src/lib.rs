//! Pond API — REST routes for Goose In A Pond
//!
//! All routes are versioned under `/api/v1/`.
//!
//! # Route plan
//!
//! ## Onboarding
//! - `POST /api/v1/handshake`  — GIAP ↔ GOTG handshake
//! - `POST /api/v1/onboard`    — Start onboarding flow
//! - `GET  /api/v1/onboard/status` — Check onboarding state
//!
//! ## Chat / Agent
//! - `POST /api/v1/chat`       — Send a message, get a response
//! - `GET  /api/v1/sessions`   — List sessions
//!
//! ## System
//! - `GET  /api/v1/health`     — Health check
//! - `GET  /api/v1/system/info` — System info (hostname, version, etc.)
//!
//! ## Devices
//! - `GET  /api/v1/devices`    — List registered devices
//! - `POST /api/v1/devices`    — Register a new device
//!
//! ## Settings
//! - `GET  /api/v1/settings`   — Get current settings
//! - `PUT  /api/v1/settings`   — Update settings
//!
//! # TODO
//! - [ ] Implement all route handlers
//! - [ ] Add authentication middleware (handshake token)
//! - [ ] Add request validation
//! - [ ] Add OpenAPI docs (utoipa)
//! - [ ] Add rate limiting
//! - [ ] Add WebSocket endpoint for streaming agent responses
//! - [ ] Serve the web dashboard at `/{route_name}`

pub mod routes;

use axum::Router;
use pond_infra::db::Database;
use std::sync::Arc;

/// Shared application state available to all route handlers.
pub struct AppState {
    pub db: Arc<Database>,
    // TODO: Add LlmProvider, ChatService, etc.
}

/// Build the full API router.
///
/// Web dashboard: `/{route_name}`
/// REST API:      `/api/v1/{route_name}`
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .nest("/api/v1", routes::api_routes())
        .nest("/", routes::web_routes())
        .with_state(state)
}
