//! Integration tests — verifies protected routes are blocked before onboarding
//!
//! Run: cargo test -p pond-api --test onboarding_integration_test

use std::sync::Arc;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use tower::ServiceExt;

use pond_core::ports::onboarding::OnboardingRepository;
use pond_core::domain::onboarding::OnboardingStep;
use pond_api::{AppState, build_router};

// ─────────────────────────────────────────────────────────────────
// Minimal mock
// ─────────────────────────────────────────────────────────────────

struct MockRepo {
    step: std::sync::Mutex<Option<OnboardingStep>>,
}

impl MockRepo {
    fn new(step: Option<OnboardingStep>) -> Self {
        Self { step: std::sync::Mutex::new(step) }
    }
}

#[async_trait::async_trait]
impl OnboardingRepository for MockRepo {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        self.step.lock().ok().and_then(|g| *g)
    }

    async fn save_step(&self, step: OnboardingStep) -> anyhow::Result<()> {
        *self.step.lock().unwrap() = Some(step);
        Ok(())
    }
}

async fn app_with_step(step: Option<OnboardingStep>) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();

    let state = Arc::new(AppState {
        db: Arc::new(db),
        onboarding_repo: Arc::new(MockRepo::new(step)) as Arc<dyn OnboardingRepository + Send + Sync>,
    });
    (build_router(state), tmp)
}

// ─────────────────────────────────────────────────────────────────
// Public routes — must always be accessible
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_is_accessible_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn onboard_status_is_accessible_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/onboard/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn system_info_is_accessible_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/system/info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

// ─────────────────────────────────────────────────────────────────
// Protected routes — must be blocked before onboarding
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn chat_is_blocked_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().method("POST").uri("/api/v1/chat").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn devices_is_blocked_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/devices").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn settings_is_blocked_before_onboarding() {
    let (app, _tmp) = app_with_step(None).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

// ─────────────────────────────────────────────────────────────────
// Protected routes — must be accessible after onboarding
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn chat_is_accessible_after_onboarding() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(Request::builder().method("POST").uri("/api/v1/chat").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn devices_is_accessible_after_onboarding() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/devices").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn settings_is_accessible_after_onboarding() {
    let (app, _tmp) = app_with_step(Some(OnboardingStep::Completed)).await;
    let res: Response = app
        .oneshot(Request::builder().uri("/api/v1/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
}
