//! Require Onboarding Completion Middleware
//!
//! Blocks access to protected API routes until onboarding is completed.

use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use serde_json::json;
use std::sync::Arc;

use crate::AppState;

use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::services::onboarding::OnboardingService;

/// Checks whether onboarding is complete.
/// If onboarding is NOT completed:
///     returns HTTP 403
/// If onboarding IS completed:
///     request proceeds to next handler
pub async fn require_onboarding_complete(
    State(state): State<Arc<AppState>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, Response> {
    if state.skip_onboarding {
        return Ok(next.run(req).await);
    }

    let service = OnboardingService::new(state.onboarding_repo.clone());

    // Get onboarding status
    let step: Option<OnboardingStep> = service.status().await;

    match step {
        // Explicitly completed → allow
        Some(OnboardingStep::Completed) => Ok(next.run(req).await),

        // None (not started) or any in-progress step → block
        None | Some(_) => Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "onboarding_required",
                "message": "Complete onboarding before using this feature"
            })),
        )
            .into_response()),
    }
}
