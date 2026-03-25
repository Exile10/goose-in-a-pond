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
//! # Authentication
//! Protected routes require a bearer token in the Authorization header:
//! ```text
//! Authorization: Bearer <token>
//! ```
//!
//! Get a token via POST /api/v1/handshake
//!
//! # Rate Limiting
//! All clients are rate limited to 100 requests per 60 seconds.

pub mod middleware;
pub mod routes;

use axum::{middleware::Next, Router};
use pond_core::ports::handshake::Handshake;
use pond_core::ports::onboarding::OnboardingRepository;
use pond_infra::db::Database;
use std::sync::Arc;

/// Shared application state available to all route handlers.
pub struct AppState {
    pub db: Arc<Database>,
    pub handshake: Arc<dyn Handshake>,
    pub onboarding_repo: Arc<dyn OnboardingRepository + Send + Sync>,
    /// Base URL of the whisper.cpp server (e.g. "http://127.0.0.1:9000").
    pub whisper_url: String,
    // TODO: Add LlmProvider, ChatService, etc.
}

/// Build the full API router.
///
/// Web dashboard: `/{route_name}`
/// REST API:      `/api/v1/{route_name}`
pub fn build_router(state: Arc<AppState>) -> Router {
    // Create rate limiter: 100 requests per 60 seconds per client
    let rate_limiter = Arc::new(middleware::RateLimiter::new(
        100,
        std::time::Duration::from_secs(60),
    ));

    Router::new()
        .nest("/api/v1", routes::api_routes(state.clone()))
        .merge(routes::web_routes())
        // Apply rate limiting to all routes
        .layer(axum::middleware::from_fn(move |req, next| {
            let limiter = rate_limiter.clone();
            rate_limit_with_limiter(req, next, limiter)
        }))
        .with_state(state)
}

async fn rate_limit_with_limiter(
    req: axum::extract::Request,
    next: Next,
    limiter: Arc<middleware::RateLimiter>,
) -> Result<axum::response::Response, middleware::AuthError> {
    // Extract client IP from ConnectInfo if available
    let client_ip = req
        .extensions()
        .get::<std::net::SocketAddr>()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    if !limiter.check_rate_limit(&client_ip).await {
        return Err(middleware::AuthError::RateLimitExceeded);
    }
    Ok(next.run(req).await)
}
