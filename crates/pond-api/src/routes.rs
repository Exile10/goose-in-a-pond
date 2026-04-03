//! Route definitions for GIAP REST API and web dashboard.
//!
//! # TODO
//! - [ ] Implement each handler with real logic
//! - [ ] Add request/response types in pond-core domain
//! - [ ] Serve static web dashboard files

use axum::{
    extract::{rejection::JsonRejection, Multipart, Path, State},
    http::StatusCode,
    response::{Html, Json},
    routing::{delete, get, patch, post},
    Router,
};
use pond_core::domain::message::ChatMessage;
use pond_core::domain::profile::CreateProfileRequest;
use pond_core::domain::sensor::{CameraEvent, SensorReading};
use pond_core::ports::scheduler::CreateTaskRequest;
use pond_core::domain::settings::Settings;
use pond_core::ports::device_registry::RegisterDeviceRequest;
use tower_http::services::ServeDir;
use pond_core::domain::onboarding::OnboardingStep;
use pond_core::ports::handshake::{HandshakeRequest, HandshakeResponse};
use pond_core::prompts::{build_system_prompt, render_template, sanitize_field, SYSTEM_PROMPT};
use pond_core::services::chat::ChatService;
use pond_core::services::onboarding::OnboardingService;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

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
        .route("/onboard/complete", post(complete_onboarding))
        .route("/onboard/status", get(onboarding_status))
        // Transcription proxy (public — local test tool)
        .route("/transcribe", post(transcribe))
        .route("/system/info", get(system_info))
        // Service connectivity test (public — diagnostic tool)
        .route("/test", get(test_services))
        .route("/test/speak", post(test_speak))
        // Goose agent status (public — dev diagnostic)
        .route("/dev/goose", get(goose_status));

    // ───────────── Protected routes (require onboarding) ─────────────
    let protected_routes = Router::new()
        .route("/chat", post(chat))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{session_id}", patch(rename_session))
        .route("/devices", get(list_devices).post(register_device))
        .route("/devices/{id}", axum::routing::delete(unregister_device))
        .route("/devices/{id}/heartbeat", post(device_heartbeat))
        .route("/settings", get(get_settings).put(update_settings))
        .route("/models", get(list_models))
        .route("/models/registry/refresh", post(refresh_model_registry))
        .route("/models/{category}/{name}/download", post(download_model))
        .route("/profiles", get(list_profiles).post(create_profile))
        .route("/profiles/{id}", get(get_profile).patch(update_profile_prefs).delete(delete_profile))
        .route("/sensors", post(record_sensor))
        .route("/sensors/{device_id}", get(get_recent_sensors))
        .route("/camera/events", get(list_camera_events).post(record_camera_event))
        .route("/camera/events/{id}/acknowledge", patch(acknowledge_camera_event))
        // ── Scheduler ──────────────────────────────────────────────────────────
        .route("/schedules", get(list_schedules).post(create_schedule))
        .route("/schedules/{id}", delete(delete_schedule))
        .route("/schedules/{id}/pause", post(pause_schedule))
        .route("/schedules/{id}/resume", post(resume_schedule))
        .route("/schedules/{id}/run-now", post(run_schedule_now))
        // ── Extensions (MCP/Goose extension manager) ───────────────────────────
        .route("/extensions", get(list_extensions_handler).post(add_extension_handler))
        .route("/extensions/{name}", delete(remove_extension_handler))
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

/// Mark onboarding as complete (public).
///
/// Called by the web UI on the final onboarding step. Saving
/// `OnboardingStep::Completed` lifts the onboarding guard middleware so that
/// protected routes become accessible. This must be called and must succeed
/// before the client attempts any authenticated request; if it fails the UI
/// shows an error and does not navigate to the dashboard.
async fn complete_onboarding(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state
        .onboarding_repo
        .save_step(OnboardingStep::Completed)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to complete onboarding: {}", e)})),
            )
        })?;
    Ok(Json(json!({"status": "completed"})))
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

#[derive(Deserialize)]
struct ChatRequest {
    session_id: Option<String>,
    message: String,
}

/// Send a message and get a response.
///
/// Creates a new session if `session_id` is not provided.
/// Persists both user and assistant messages to session storage.
async fn chat(
    State(state): State<Arc<AppState>>,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;

    let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let storage = &state.session_storage;

    // Ensure session exists
    if storage.get_session(&session_id).await.is_err() {
        storage
            .create_session(session_id.clone())
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("Failed to create session: {}", e)})),
                )
            })?;
    }

    // Build the system prompt: user-supplied template file takes priority over
    // the settings-based builder.  Mirrors Goose's `prompts/system.md` override.
    let system_prompt = {
        let settings = state.settings_repo.get().await.ok();

        // Try to load $DATA_DIR/prompts/system.md override
        let file_template = state
            .prompt_template_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join("system.md")).ok());

        match (file_template, settings) {
            (Some(tmpl), Some(s)) => {
                // Pre-compute sanitized strings so temporaries outlive the borrow
                let name     = sanitize_field(&s.assistant_name, 50);
                let user     = sanitize_field(&s.user_name, 50);
                let persona  = sanitize_field(&s.assistant_personality, 200);
                let tz       = sanitize_field(&s.timezone, 50);
                render_template(&tmpl, &[
                    ("assistant_name", name.as_str()),
                    ("user_name",      user.as_str()),
                    ("personality",    persona.as_str()),
                    ("timezone",       tz.as_str()),
                ])
            }
            (None, Some(s)) => build_system_prompt(
                &s.assistant_name,
                &s.user_name,
                &s.assistant_personality,
                &s.timezone,
            ),
            _ => SYSTEM_PROMPT.to_string(),
        }
    };

    // Append MCP memory context to the system prompt when available.
    let system_prompt = match &state.mcp_memory {
        Some(m) => {
            let mem = m.instructions();
            if mem.is_empty() {
                system_prompt
            } else {
                format!("{}\n\n---\n{}", system_prompt, mem)
            }
        }
        None => system_prompt,
    };

    // Build ChatService — wires LLM provider when available, falls back to agent
    let mut service = ChatService::new(
        state.agent.clone(),
        session_id.clone(),
        storage.clone(),
    )
    .with_system_prompt(system_prompt);
    if let Some(provider) = &state.llm_provider {
        service = service.with_provider(provider.clone());
    }

    let response_text = service
        .chat_once(req.message)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
        })?;

    Ok(Json(json!({
        "session_id": session_id,
        "response": response_text,
    })))
}

/// List all sessions, ordered by most recently updated first.
async fn list_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let sessions = state
        .session_storage
        .list_sessions()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to list sessions: {}", e)})),
            )
        })?;

    let session_list: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "title": s.title,
                "created_at": s.created_at.to_rfc3339(),
                "updated_at": s.updated_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(json!({ "sessions": session_list })))
}

#[derive(Deserialize)]
struct RenameSessionRequest {
    title: String,
}

/// Rename a session (set or update its title).
///
/// PATCH /api/v1/sessions/:session_id
/// Body: { "title": "New Title" }
async fn rename_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    body: Result<Json<RenameSessionRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid request: {}", e)})),
        )
    })?;

    state
        .session_storage
        .update_title(&session_id, req.title.clone())
        .await
        .map_err(|e| {
            let status = match &e {
                pond_core::ports::session_storage::SessionStorageError::SessionNotFound(_) => {
                    StatusCode::NOT_FOUND
                }
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({"error": format!("{}", e)})))
        })?;

    Ok(Json(json!({
        "session_id": session_id,
        "title": req.title,
    })))
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

async fn list_devices(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let devices = state.device_registry.list_devices().await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    let list: Vec<Value> = devices
        .iter()
        .map(|d| json!({
            "id":            d.id,
            "name":          d.name,
            "device_type":   d.device_type,
            "hostname":      d.hostname,
            "ip_address":    d.ip_address,
            "capabilities":  d.capabilities,
            "registered_at": d.registered_at,
            "last_seen":     d.last_seen,
            "is_online":     d.is_online,
        }))
        .collect();
    Ok(Json(json!({ "devices": list })))
}

async fn register_device(
    State(state): State<Arc<AppState>>,
    body: Result<Json<RegisterDeviceRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid request: {}", e)})))
    })?;
    let device = state.device_registry.register(req).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok((StatusCode::CREATED, Json(json!({
        "id":            device.id,
        "name":          device.name,
        "device_type":   device.device_type,
        "capabilities":  device.capabilities,
        "registered_at": device.registered_at,
        "is_online":     device.is_online,
    }))))
}

async fn unregister_device(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    state.device_registry.unregister(&id).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn device_heartbeat(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state.device_registry.heartbeat(&id).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(Json(json!({ "status": "ok" })))
}

async fn get_settings(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let settings = state
        .settings_repo
        .get()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to load settings: {}", e)})),
            )
        })?;
    Ok(Json(serde_json::to_value(settings).unwrap_or(json!({}))))
}

async fn update_settings(
    State(state): State<Arc<AppState>>,
    body: Result<Json<Settings>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(new_settings) = body.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid settings body: {}", e)})),
        )
    })?;

    state
        .settings_repo
        .update(&new_settings)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to save settings: {}", e)})),
            )
        })?;

    Ok(Json(json!({ "status": "ok" })))
}

// ── Model registry handlers ───────────────────────────────────────────────────

/// GET /api/v1/models — returns the model status snapshot with downloaded/active flags.
async fn list_models(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(model_status) = &state.model_status else {
        return Ok(Json(json!({"whisper": [], "llamafile": [], "tts": []})));
    };
    let entries = model_status.read().await.clone();

    // Group by category for a tidy response shape.
    let mut whisper   = vec![];
    let mut llamafile = vec![];
    let mut tts       = vec![];

    for e in entries {
        let v = serde_json::to_value(&e).unwrap_or_default();
        match e.category.as_str() {
            "whisper"   => whisper.push(v),
            "llamafile" => llamafile.push(v),
            "tts"       => tts.push(v),
            _           => {}
        }
    }

    Ok(Json(json!({"whisper": whisper, "llamafile": llamafile, "tts": tts})))
}

/// POST /api/v1/models/registry/refresh — fetch the latest registry from the online URL.
async fn refresh_model_registry(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(model_status) = &state.model_status else {
        return Ok(Json(json!({"status": "no_registry"})));
    };

    let settings = state.settings_repo.get().await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;

    let data_dir = state.data_dir.clone().unwrap_or_else(|| std::path::PathBuf::from("."));
    let url = settings.model_registry_url.clone();

    // Fetch in the background so we don't block on slow network.
    let status_lock = Arc::clone(model_status);
    let active_w = settings.active_whisper_model.clone();
    let active_l = settings.active_llm_model.clone();
    let active_t = settings.active_tts_model.clone();

    tokio::spawn(async move {
        match reqwest::get(&url).await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(bytes) = resp.bytes().await {
                    if let Ok(registry) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        // Save cache
                        let _ = std::fs::write(data_dir.join("registry.json"), &bytes);
                        tracing::info!("Model registry refreshed from {}", url);

                        // Rebuild status snapshot from fresh registry
                        if let Ok(typed) = serde_json::from_value::<ModelRegistrySnapshot>(registry) {
                            let new_status = typed.build_status(&active_w, &active_l, &active_t, &data_dir);
                            *status_lock.write().await = new_status;
                        }
                    }
                }
            }
            Ok(resp) => tracing::warn!("Registry refresh returned {}", resp.status()),
            Err(e)   => tracing::warn!("Registry refresh failed: {}", e),
        }
    });

    Ok(Json(json!({"status": "refresh_started"})))
}

/// POST /api/v1/models/{category}/{name}/download — download a specific model.
async fn download_model(
    State(state): State<Arc<AppState>>,
    Path((category, name)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(model_status) = &state.model_status else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "registry not available"}))));
    };

    // Check if already downloaded.
    {
        let entries = model_status.read().await;
        if let Some(e) = entries.iter().find(|e| e.category == category && e.name == name) {
            if e.downloaded {
                return Ok(Json(json!({"status": "already_downloaded", "name": name})));
            }
        } else {
            return Err((StatusCode::NOT_FOUND, Json(json!({"error": format!("Model '{}' not found in category '{}'", name, category)}))));
        }
    }

    // TODO: wire download_model_entry() once model_download is accessible from pond-api.
    // For now return accepted — the download should be triggered from the server side.
    Ok(Json(json!({
        "status": "download_not_supported_via_api",
        "hint": "Run `pond-server setup` to download models, or use the settings to change active models"
    })))
}

/// Minimal registry shape needed to rebuild status inside routes.
/// This avoids pulling pond-server internals into pond-api.
#[derive(serde::Deserialize)]
pub struct ModelRegistrySnapshot {
    pub whisper:   Vec<RegistryWhisperEntry>,
    pub llamafile: Vec<RegistryLlamafileEntry>,
    pub tts:       Vec<serde_json::Value>,
}

impl ModelRegistrySnapshot {
    fn build_status(&self, active_w: &str, active_l: &str, active_t: &str, data_dir: &std::path::Path) -> Vec<crate::ModelStatusEntry> {
        let mut out = Vec::new();
        for m in &self.whisper {
            out.push(crate::ModelStatusEntry {
                category: "whisper".into(), name: m.name.clone(),
                description: m.description.clone(), size_mb: m.size_mb,
                downloaded: data_dir.join("models").join(&m.filename).exists(),
                active: m.name == active_w,
            });
        }
        for m in &self.llamafile {
            #[cfg(windows)]
            let path = std::path::PathBuf::from(format!("{}.exe", data_dir.join("models").join("llm").join(&m.filename).display()));
            #[cfg(not(windows))]
            let path = data_dir.join("models").join("llm").join(&m.filename);
            out.push(crate::ModelStatusEntry {
                category: "llamafile".into(), name: m.name.clone(),
                description: m.description.clone(), size_mb: m.size_mb,
                downloaded: path.exists(), active: m.name == active_l,
            });
        }
        for entry in &self.tts {
            let name = entry["name"].as_str().unwrap_or("").to_string();
            let engine = entry["engine"].as_str().unwrap_or("");
            let downloaded = if engine == "http" {
                true
            } else {
                let fname = entry["model_filename"].as_str().unwrap_or("");
                data_dir.join("models").join("tts").join(fname).exists()
            };
            out.push(crate::ModelStatusEntry {
                category: "tts".into(), name: name.clone(),
                description: entry["description"].as_str().unwrap_or("").to_string(),
                size_mb: entry["size_mb"].as_u64().unwrap_or(0),
                downloaded, active: name == active_t,
            });
        }
        out
    }
}

#[derive(serde::Deserialize)]
pub struct RegistryWhisperEntry   { pub name: String, pub filename: String, pub description: String, pub size_mb: u64 }
#[derive(serde::Deserialize)]
pub struct RegistryLlamafileEntry { pub name: String, pub filename: String, pub description: String, pub size_mb: u64 }

// ── Profile handlers ──────────────────────────────────────────────────────────

async fn list_profiles(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let profiles = state.profile_repo.list().await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    let list: Vec<Value> = profiles
        .iter()
        .map(|p| json!({
            "id":           p.id,
            "display_name": p.display_name,
            "avatar_emoji": p.avatar_emoji,
            "preferences":  p.preferences,
            "created_at":   p.created_at.to_rfc3339(),
            "updated_at":   p.updated_at.to_rfc3339(),
        }))
        .collect();
    Ok(Json(json!({ "profiles": list })))
}

async fn create_profile(
    State(state): State<Arc<AppState>>,
    body: Result<Json<CreateProfileRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid request: {}", e)})))
    })?;
    let profile = state.profile_repo.create(req).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok((StatusCode::CREATED, Json(json!({
        "id":           profile.id,
        "display_name": profile.display_name,
        "avatar_emoji": profile.avatar_emoji,
        "preferences":  profile.preferences,
        "created_at":   profile.created_at.to_rfc3339(),
        "updated_at":   profile.updated_at.to_rfc3339(),
    }))))
}

async fn get_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let profile = state.profile_repo.get(&id).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    match profile {
        Some(p) => Ok(Json(json!({
            "id":           p.id,
            "display_name": p.display_name,
            "avatar_emoji": p.avatar_emoji,
            "preferences":  p.preferences,
            "created_at":   p.created_at.to_rfc3339(),
            "updated_at":   p.updated_at.to_rfc3339(),
        }))),
        None => Err((StatusCode::NOT_FOUND, Json(json!({"error": "profile not found"})))),
    }
}

#[derive(serde::Deserialize)]
struct UpdatePrefsRequest {
    preferences: std::collections::HashMap<String, String>,
}

async fn update_profile_prefs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<UpdatePrefsRequest>, JsonRejection>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid request: {}", e)})))
    })?;
    let profile = state
        .profile_repo
        .update_preferences(&id, req.preferences)
        .await
        .map_err(|e| {
            let status = if e.to_string().contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(json!({"error": e.to_string()})))
        })?;
    Ok(Json(json!({
        "id":           profile.id,
        "display_name": profile.display_name,
        "preferences":  profile.preferences,
        "updated_at":   profile.updated_at.to_rfc3339(),
    })))
}

async fn delete_profile(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    state.profile_repo.delete(&id).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Sensor handlers ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct SensorReadingRequest {
    device_id:   String,
    sensor_type: String,
    value:       f64,
    unit:        String,
}

async fn record_sensor(
    State(state): State<Arc<AppState>>,
    body: Result<Json<SensorReadingRequest>, JsonRejection>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid request: {}", e)})))
    })?;
    let reading = SensorReading {
        device_id:   req.device_id,
        sensor_type: req.sensor_type,
        value:       req.value,
        unit:        req.unit,
        recorded_at: chrono::Utc::now(),
    };
    state.sensor_storage.record(reading).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(StatusCode::CREATED)
}

#[derive(serde::Deserialize)]
struct SensorQueryParams {
    limit: Option<usize>,
}

async fn get_recent_sensors(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<SensorQueryParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let limit = params.limit.unwrap_or(20).min(100);
    let readings = state
        .sensor_storage
        .get_recent(&device_id, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    let list: Vec<Value> = readings
        .iter()
        .map(|r| json!({
            "device_id":   r.device_id,
            "sensor_type": r.sensor_type,
            "value":       r.value,
            "unit":        r.unit,
            "recorded_at": r.recorded_at.to_rfc3339(),
        }))
        .collect();
    Ok(Json(json!({ "readings": list })))
}

// ── Camera handlers ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct CameraEventRequest {
    camera_id:     String,
    event_type:    String,
    confidence:    Option<f64>,
    snapshot_path: Option<String>,
    metadata:      Option<String>,
}

#[derive(serde::Deserialize)]
struct CameraQueryParams {
    camera_id: Option<String>,
    limit:     Option<usize>,
}

async fn record_camera_event(
    State(state): State<Arc<AppState>>,
    body: Result<Json<CameraEventRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let Json(req) = body.map_err(|e| {
        (StatusCode::BAD_REQUEST, Json(json!({"error": format!("Invalid request: {}", e)})))
    })?;
    let event = CameraEvent {
        id:            None,
        camera_id:     req.camera_id,
        event_type:    req.event_type,
        confidence:    req.confidence,
        snapshot_path: req.snapshot_path,
        metadata:      req.metadata,
        acknowledged:  false,
        created_at:    chrono::Utc::now(),
    };
    let id = state.camera_storage.record_event(event).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn list_camera_events(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<CameraQueryParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let camera_id = params.camera_id.as_deref().unwrap_or("default");
    let limit = params.limit.unwrap_or(20).min(100);
    let events = state
        .camera_storage
        .list_events(camera_id, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    let list: Vec<Value> = events
        .iter()
        .map(|e| json!({
            "id":             e.id,
            "camera_id":      e.camera_id,
            "event_type":     e.event_type,
            "confidence":     e.confidence,
            "snapshot_path":  e.snapshot_path,
            "acknowledged":   e.acknowledged,
            "created_at":     e.created_at.to_rfc3339(),
        }))
        .collect();
    Ok(Json(json!({ "events": list })))
}

async fn acknowledge_camera_event(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state.camera_storage.acknowledge(id).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(Json(json!({ "status": "ok" })))
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

// ── Service connectivity test ─────────────────────────────────────────────────

/// `GET /api/v1/test`
///
/// Probes all external services in parallel and returns their status.
/// Use this to confirm whisper, llamafile, and ollama are reachable before
/// starting a voice session.
///
/// Response shape:
/// ```json
/// {
///   "whisper":   { "status": "ok",          "url": "...", "latency_ms": 12 },
///   "llamafile": { "status": "unavailable",  "url": "...", "error": "connection refused" },
///   "ollama":    { "status": "unavailable",  "url": "...", "error": "..." },
///   "llm":       { "status": "ok",           "provider": "llamafile → ollama", "response": "pong", "latency_ms": 220 }
/// }
/// ```
async fn test_services(State(state): State<Arc<AppState>>) -> Json<Value> {
    let client = &state.http_client;

    // Run all connectivity checks concurrently.
    let (whisper_result, llamafile_result, ollama_result) = tokio::join!(
        probe(client, &state.whisper_url, 3),
        probe(client, "http://127.0.0.1:8080", 3),
        probe(client, "http://127.0.0.1:11434", 3),
    );

    // Optionally probe the wired LLM provider with a real completion.
    let llm_result = if let Some(provider) = &state.llm_provider {
        let model_name = provider.model_name();
        let t0 = std::time::Instant::now();
        let res = provider
            .complete(
                "You are a test service. Reply with exactly one word.",
                vec![ChatMessage::user("pong")],
            )
            .await;
        let ms = t0.elapsed().as_millis() as u64;
        match res {
            Ok(msg) => json!({
                "status": "ok",
                "provider": model_name,
                "response": msg.content.trim(),
                "latency_ms": ms,
            }),
            Err(e) => json!({
                "status": "error",
                "provider": model_name,
                "error": e.to_string(),
            }),
        }
    } else {
        json!({ "status": "not_configured" })
    };

    Json(json!({
        "whisper":   whisper_result,
        "llamafile": llamafile_result,
        "ollama":    ollama_result,
        "llm":       llm_result,
    }))
}

// ── Dev test HTML page ────────────────────────────────────────────────────────

const DEV_TEST_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>GIAP Dev Test</title>
<style>
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:monospace;background:#0d1117;color:#c9d1d9;padding:2rem}
h1{color:#58a6ff;margin-bottom:0.5rem}
.subtitle{color:#8b949e;font-size:0.8rem;margin-bottom:1.5rem}
h2{color:#79c0ff;font-size:0.95rem;margin-bottom:0.75rem}
.card{background:#161b22;border:1px solid #30363d;border-radius:6px;padding:1.25rem;margin-bottom:1.25rem}
.row{display:flex;gap:0.75rem;align-items:center;margin-bottom:0.5rem}
label{color:#8b949e;font-size:0.82rem;min-width:110px}
.badge{display:inline-block;padding:2px 10px;border-radius:12px;font-size:0.78rem;font-weight:bold}
.ok{background:#1a4731;color:#56d364}
.unavailable{background:#3d1a1a;color:#f85149}
.pending{background:#2d2a1e;color:#d29922}
.unknown{background:#21262d;color:#8b949e}
button{background:#238636;color:#fff;border:none;border-radius:4px;padding:5px 14px;cursor:pointer;font-family:monospace;font-size:0.85rem}
button:hover{background:#2ea043}
button:disabled{background:#333;color:#555;cursor:not-allowed}
button.danger{background:#b91c1c}
button.danger:hover{background:#dc2626}
button.danger.pulse{animation:pulse 1s infinite}
button.secondary{background:#21262d;border:1px solid #30363d}
button.secondary:hover{background:#30363d}
textarea,input[type=text]{width:100%;background:#0d1117;border:1px solid #30363d;border-radius:4px;color:#c9d1d9;padding:7px;font-family:monospace;font-size:0.85rem}
textarea{height:70px;resize:vertical}
pre{background:#0d1117;border:1px solid #21262d;border-radius:4px;padding:10px;font-size:0.78rem;overflow:auto;white-space:pre-wrap;word-break:break-all;max-height:180px;margin-top:0.5rem}
.ms{color:#8b949e;font-size:0.78rem}
.grid{display:grid;grid-template-columns:1fr 1fr;gap:1.25rem}
.note{color:#8b949e;font-size:0.78rem;margin-bottom:0.75rem}
@media(max-width:750px){.grid{grid-template-columns:1fr}}
@keyframes pulse{0%,100%{background:#b91c1c}50%{background:#ef4444}}
.flex{display:flex;gap:0.5rem;align-items:center;margin-bottom:0.75rem}
.detected{color:#56d364;font-weight:bold}
.not-detected{color:#f85149;font-weight:bold}
code{background:#21262d;padding:1px 5px;border-radius:3px;font-size:0.8rem}
</style>
</head>
<body>
<h1>🦆 GIAP Dev Test Panel</h1>
<p class="subtitle">⚠ Testing only — never expose this page to the internet.</p>

<!-- Services status -->
<div class="card">
  <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:0.75rem">
    <h2>Services</h2>
    <button onclick="checkServices()">Refresh</button>
  </div>
  <div style="display:grid;grid-template-columns:1fr 1fr;gap:0.25rem 1rem">
    <div class="row"><label>Whisper</label><span id="svc-whisper" class="badge unknown">—</span><span id="svc-whisper-ms" class="ms"></span></div>
    <div class="row"><label>Llamafile</label><span id="svc-llamafile" class="badge unknown">—</span><span id="svc-llamafile-ms" class="ms"></span></div>
    <div class="row"><label>Ollama</label><span id="svc-ollama" class="badge unknown">—</span><span id="svc-ollama-ms" class="ms"></span></div>
    <div class="row"><label>LLM Provider</label><span id="svc-llm" class="badge unknown">—</span><span id="svc-llm-ms" class="ms"></span></div>
  </div>
  <pre id="svc-detail" style="margin-top:0.5rem;display:none"></pre>
</div>

<div class="grid">
  <!-- Chat test -->
  <div class="card">
    <h2>Chat</h2>
    <p class="note">Requires onboarding completed. <button class="secondary" style="padding:2px 8px;font-size:0.75rem" onclick="doOnboard()">Auto-onboard</button></p>
    <div style="margin-bottom:0.5rem">
      <input type="text" id="chat-session" placeholder="session_id (blank = new)" style="margin-bottom:6px">
      <textarea id="chat-msg">What is 2+2?</textarea>
    </div>
    <div class="flex">
      <button onclick="sendChat()">Send</button>
      <button class="secondary" onclick="clearChat()">Clear</button>
    </div>
    <pre id="chat-out">—</pre>
  </div>

  <!-- Whisper transcription -->
  <div class="card">
    <h2>Whisper Transcription</h2>
    <p class="note">Records mic audio → <code>/api/v1/transcribe</code> → whisper.cpp</p>
    <div class="flex">
      <button id="rec-btn" onclick="toggleRecording()">● Record</button>
      <span id="rec-status" style="font-size:0.8rem;color:#8b949e"></span>
    </div>
    <pre id="transcribe-out">—</pre>
  </div>

  <!-- Wake word test -->
  <div class="card">
    <h2>Wake Word Test</h2>
    <p class="note">Records 3 s → transcribes → looks for <code>"goose"</code> in transcript</p>
    <div class="flex">
      <button id="wake-btn" onclick="testWakeWord()">▶ Test (3 s)</button>
      <span id="wake-status" style="font-size:0.8rem;color:#8b949e"></span>
    </div>
    <pre id="wake-out">Say "Goose" during the recording window.</pre>
  </div>

  <!-- Fallback provider -->
  <div class="card">
    <h2>Fallback Provider</h2>
    <p class="note">Sends a simple prompt through the wired LLM provider (includes fallback chain if configured).</p>
    <div class="flex">
      <button onclick="testFallback()">Test</button>
    </div>
    <pre id="fallback-out">—</pre>
  </div>

  <!-- TTS / Piper -->
  <div class="card">
    <h2>TTS (Piper)</h2>
    <p class="note">Plays audio on the <strong>server device</strong> speaker via <code>/api/v1/test/speak</code>. Requires server started with <code>--tts piper</code>.</p>
    <div style="margin-bottom:0.5rem">
      <textarea id="tts-text">Hello! I am Goose, your local AI assistant.</textarea>
    </div>
    <div class="flex">
      <button onclick="testTts()">▶ Speak on device</button>
    </div>
    <pre id="tts-out">—</pre>
  </div>

  <!-- Weather -->
  <div class="card" style="grid-column:1/-1">
    <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:0.75rem">
      <h2>Weather</h2>
      <div style="display:flex;gap:0.5rem;align-items:center">
        <span id="wx-cfg-badge" class="badge unknown">—</span>
        <button class="secondary" onclick="fetchWeather()">↺ Fetch</button>
        <button class="secondary" onclick="geolocate()">📍 My location</button>
      </div>
    </div>
    <p class="note">
      Weather is injected into every LLM system prompt when enabled.
      Enable via <code>PUT /api/v1/settings</code> with <code>weather_enabled:true</code>, <code>weather_latitude</code>, <code>weather_longitude</code>, <code>weather_location_name</code>.
    </p>
    <div style="display:grid;grid-template-columns:1fr 1fr 2fr;gap:0.5rem;margin-bottom:0.75rem">
      <div><label style="display:block;margin-bottom:2px;font-size:0.8rem">Latitude</label><input type="text" id="wx-lat" placeholder="-1.286"></div>
      <div><label style="display:block;margin-bottom:2px;font-size:0.8rem">Longitude</label><input type="text" id="wx-lon" placeholder="36.817"></div>
      <div><label style="display:block;margin-bottom:2px;font-size:0.8rem">Location name</label><input type="text" id="wx-loc" placeholder="Nairobi, KE"></div>
    </div>
    <div class="flex" style="margin-bottom:0.75rem">
      <button onclick="testWeatherDirect()">▶ Test (direct API call)</button>
      <button class="secondary" onclick="saveWeatherSettings()">💾 Save &amp; enable</button>
      <button class="secondary" onclick="disableWeather()">✕ Disable</button>
    </div>
    <div id="wx-display" style="display:none">
      <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(130px,1fr));gap:0.5rem;margin-bottom:0.75rem">
        <div style="background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:0.6rem;text-align:center">
          <div style="font-size:1.6rem;font-weight:bold;color:#79c0ff" id="wx-temp">—</div>
          <div style="font-size:0.7rem;color:#8b949e">Temperature</div>
        </div>
        <div style="background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:0.6rem;text-align:center">
          <div style="font-size:1.6rem;font-weight:bold;color:#79c0ff" id="wx-feels">—</div>
          <div style="font-size:0.7rem;color:#8b949e">Feels like</div>
        </div>
        <div style="background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:0.6rem;text-align:center">
          <div style="font-size:1.6rem;font-weight:bold;color:#79c0ff" id="wx-hum">—</div>
          <div style="font-size:0.7rem;color:#8b949e">Humidity</div>
        </div>
        <div style="background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:0.6rem;text-align:center">
          <div style="font-size:1.6rem;font-weight:bold;color:#79c0ff" id="wx-wind">—</div>
          <div style="font-size:0.7rem;color:#8b949e">Wind</div>
        </div>
        <div style="background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:0.6rem;text-align:center">
          <div style="font-size:1.6rem;font-weight:bold;color:#79c0ff" id="wx-precip">—</div>
          <div style="font-size:0.7rem;color:#8b949e">Precipitation</div>
        </div>
      </div>
      <div id="wx-desc" style="color:#56d364;font-size:0.9rem;margin-bottom:0.5rem"></div>
      <details>
        <summary style="cursor:pointer;color:#8b949e;font-size:0.78rem">LLM context block (what the assistant sees)</summary>
        <pre id="wx-context-block" style="margin-top:0.4rem"></pre>
      </details>
    </div>
    <pre id="wx-out" style="display:none"></pre>
  </div>

  <!-- Scheduler -->
  <div class="card" style="grid-column:1/-1">
    <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:0.75rem">
      <h2>Scheduler</h2>
      <button class="secondary" onclick="listSchedules()">↺ Refresh</button>
    </div>
    <p class="note">Cron uses 6-field format: <code>sec min hour dom month dow</code> — e.g. <code>0 0 8 * * *</code> = 08:00 daily.</p>

    <!-- Create form -->
    <details style="margin-bottom:0.75rem">
      <summary style="cursor:pointer;color:#79c0ff;font-size:0.85rem;margin-bottom:0.5rem">+ Create task</summary>
      <div style="display:grid;grid-template-columns:1fr 1fr;gap:0.5rem;margin-top:0.5rem">
        <div><label style="display:block;margin-bottom:2px">ID</label><input type="text" id="sched-id" placeholder="morning-summary"></div>
        <div><label style="display:block;margin-bottom:2px">Label</label><input type="text" id="sched-label" placeholder="Morning summary"></div>
        <div><label style="display:block;margin-bottom:2px">Cron (6-field)</label><input type="text" id="sched-cron" placeholder="0 0 8 * * *"></div>
        <div><label style="display:block;margin-bottom:2px">Webhook URL (optional)</label><input type="text" id="sched-webhook" placeholder="http://localhost:9999/hook"></div>
      </div>
      <div style="margin-top:0.5rem">
        <button onclick="createSchedule()">Create</button>
      </div>
    </details>

    <!-- Task list -->
    <div id="sched-list"><p style="color:#8b949e;font-size:0.82rem">Click Refresh to load tasks.</p></div>
    <pre id="sched-out" style="display:none;margin-top:0.5rem">—</pre>
  </div>

  <!-- Goose MCP Extensions -->
  <div class="card" style="grid-column:1/-1">
    <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:0.75rem">
      <h2>Goose MCP Extensions</h2>
      <div style="display:flex;gap:0.5rem;align-items:center">
        <span id="goose-badge" class="badge unknown">—</span>
        <button class="secondary" onclick="loadGooseStatus()">↺ Refresh</button>
      </div>
    </div>
    <p class="note">Enable with: <code>cargo run -p pond-server --features goose-agent -- serve --agent goose</code>.
    Extensions require onboarding. <button class="secondary" style="padding:2px 8px;font-size:0.75rem" onclick="doOnboard()">Auto-onboard</button></p>

    <!-- Extension list -->
    <div id="goose-ext-list" style="margin-bottom:0.75rem"><p style="color:#8b949e;font-size:0.82rem">Click Refresh to load extensions.</p></div>

    <!-- Add extension -->
    <details style="margin-bottom:0.75rem">
      <summary style="cursor:pointer;color:#79c0ff;font-size:0.85rem;margin-bottom:0.5rem">+ Add extension</summary>
      <div style="display:grid;grid-template-columns:1fr 1fr;gap:0.5rem;margin-top:0.5rem">
        <div><label style="display:block;margin-bottom:2px">Kind</label>
          <select id="ext-kind" style="width:100%;background:#0d1117;border:1px solid #30363d;border-radius:4px;color:#c9d1d9;padding:7px;font-family:monospace;font-size:0.85rem" onchange="onExtKindChange()">
            <option value="builtin">builtin</option>
            <option value="stdio">stdio</option>
            <option value="streamable_http">streamable_http</option>
          </select>
        </div>
        <div><label style="display:block;margin-bottom:2px">Name</label><input type="text" id="ext-name" placeholder="giap"></div>
        <div id="ext-cmd-row"><label style="display:block;margin-bottom:2px">Command (stdio only)</label><input type="text" id="ext-cmd" placeholder="/usr/bin/my-mcp-server"></div>
        <div id="ext-uri-row" style="display:none"><label style="display:block;margin-bottom:2px">URI (http only)</label><input type="text" id="ext-uri" placeholder="http://localhost:8080/mcp"></div>
        <div style="grid-column:1/-1"><label style="display:block;margin-bottom:2px">Description</label><input type="text" id="ext-desc" placeholder="optional description"></div>
      </div>
      <div style="margin-top:0.5rem"><button onclick="addExtension()">Add</button></div>
    </details>

    <!-- Goose chat (uses existing /api/v1/chat, active only when --agent goose) -->
    <details>
      <summary style="cursor:pointer;color:#79c0ff;font-size:0.85rem;margin-bottom:0.5rem">▶ Test Goose chat</summary>
      <p class="note" style="margin-top:0.5rem">Sends through the wired agent. When <code>--agent goose</code> is active the Goose engine handles the message and can call MCP tools.</p>
      <input type="text" id="goose-session" placeholder="session_id (blank = new)" style="margin-bottom:6px">
      <textarea id="goose-msg" style="margin-bottom:6px">List my registered devices using the GIAP MCP tools.</textarea>
      <div class="flex"><button onclick="sendGooseChat()">Send</button></div>
      <pre id="goose-chat-out">—</pre>
    </details>

    <pre id="goose-out" style="display:none;margin-top:0.5rem">—</pre>
  </div>
</div>

<script>
const API='/api/v1';

// ── Onboarding ────────────────────────────────────────────────────────────────
async function doOnboard(){
  try{
    const r=await fetch(`${API}/onboard/complete`,{method:'POST'});
    const d=await r.json();
    if(d.status==='completed') location.reload();
    else alert(JSON.stringify(d));
  }catch(e){alert('Onboard error: '+e.message);}
}

// ── Services ──────────────────────────────────────────────────────────────────
async function checkServices(){
  ['whisper','llamafile','ollama','llm'].forEach(k=>setBadge(k,'pending','…'));
  try{
    const r=await fetch(`${API}/test`);
    const d=await r.json();
    renderSvc('whisper',d.whisper);
    renderSvc('llamafile',d.llamafile);
    renderSvc('ollama',d.ollama);
    renderLlm(d.llm);
    const det=document.getElementById('svc-detail');
    det.style.display='block';
    det.textContent=JSON.stringify(d,null,2);
  }catch(e){
    ['whisper','llamafile','ollama','llm'].forEach(k=>setBadge(k,'unavailable','error'));
  }
}
function renderSvc(k,s){
  if(!s)return setBadge(k,'unknown','?');
  setBadge(k,s.status==='ok'?'ok':'unavailable',s.status==='ok'?'ok':'unavailable');
  document.getElementById('svc-'+k+'-ms').textContent=s.latency_ms!=null?s.latency_ms+'ms':'';
}
function renderLlm(l){
  if(!l)return setBadge('llm','unknown','?');
  if(l.status==='not_configured')return setBadge('llm','unknown','not configured');
  setBadge('llm',l.status==='ok'?'ok':'unavailable',l.provider||l.status);
  document.getElementById('svc-llm-ms').textContent=l.latency_ms!=null?l.latency_ms+'ms':'';
}
function setBadge(k,cls,txt){
  const el=document.getElementById('svc-'+k);
  el.className='badge '+cls;el.textContent=txt;
}

// ── Chat ──────────────────────────────────────────────────────────────────────
async function sendChat(){
  const msg=document.getElementById('chat-msg').value.trim();
  if(!msg)return;
  const si=document.getElementById('chat-session');
  const out=document.getElementById('chat-out');
  out.textContent='…';
  const body={message:msg};
  if(si.value.trim())body.session_id=si.value.trim();
  try{
    const r=await fetch(`${API}/chat`,{
      method:'POST',
      headers:{'Content-Type':'application/json','Authorization':'Bearer dev'},
      body:JSON.stringify(body)
    });
    const d=await r.json();
    if(d.session_id)si.value=d.session_id;
    out.textContent=JSON.stringify(d,null,2);
  }catch(e){out.textContent='Error: '+e.message;}
}
function clearChat(){
  document.getElementById('chat-session').value='';
  document.getElementById('chat-out').textContent='—';
}

// ── WAV encoder (browser → whisper.cpp requires 16-bit PCM WAV, 16 kHz mono) ──
// Browser MediaRecorder produces WebM/Opus which whisper.cpp cannot decode.
// We decode via AudioContext, resample to 16 kHz mono, then write a WAV header.
async function blobToWav(blob){
  const ab=await blob.arrayBuffer();
  const ctx=new AudioContext();
  let decoded;
  try{decoded=await ctx.decodeAudioData(ab);}
  finally{ctx.close();}
  const SR=16000;
  const len=Math.ceil(decoded.duration*SR);
  const off=new OfflineAudioContext(1,len,SR);
  const src=off.createBufferSource();
  src.buffer=decoded;
  src.connect(off.destination);
  src.start(0);
  const rendered=await off.startRendering();
  const pcmF32=rendered.getChannelData(0);
  const pcm16=new Int16Array(pcmF32.length);
  for(let i=0;i<pcmF32.length;i++){
    const s=Math.max(-1,Math.min(1,pcmF32[i]));
    pcm16[i]=s<0?s*0x8000:s*0x7fff;
  }
  // Build WAV container
  const buf=new ArrayBuffer(44+pcm16.byteLength);
  const v=new DataView(buf);
  const str=(o,s)=>{for(let i=0;i<s.length;i++)v.setUint8(o+i,s.charCodeAt(i));};
  str(0,'RIFF');v.setUint32(4,36+pcm16.byteLength,true);str(8,'WAVE');
  str(12,'fmt ');v.setUint32(16,16,true);v.setUint16(20,1,true);v.setUint16(22,1,true);
  v.setUint32(24,SR,true);v.setUint32(28,SR*2,true);v.setUint16(32,2,true);v.setUint16(34,16,true);
  str(36,'data');v.setUint32(40,pcm16.byteLength,true);
  new Int16Array(buf,44).set(pcm16);
  return new Blob([buf],{type:'audio/wav'});
}

// ── Recording helpers ─────────────────────────────────────────────────────────
let mr=null,chunks=[];
async function toggleRecording(){
  if(mr&&mr.state==='recording'){mr.stop();return;}
  chunks=[];
  try{
    const stream=await navigator.mediaDevices.getUserMedia({audio:true});
    mr=new MediaRecorder(stream);
    mr.ondataavailable=e=>chunks.push(e.data);
    mr.onstop=async()=>{
      stream.getTracks().forEach(t=>t.stop());
      const raw=new Blob(chunks,{type:mr.mimeType});
      transcribeBlob(raw,'transcribe-out');
      setRecBtn(false);
    };
    mr.start();setRecBtn(true);
  }catch(e){document.getElementById('transcribe-out').textContent='Mic error: '+e.message;}
}
function setRecBtn(on){
  const b=document.getElementById('rec-btn');
  const s=document.getElementById('rec-status');
  if(on){b.textContent='■ Stop';b.classList.add('danger','pulse');s.textContent='Recording…';}
  else{b.textContent='● Record';b.className='';s.textContent='';}
}
async function transcribeBlob(raw,outId){
  const out=document.getElementById(outId);
  out.textContent='Converting to WAV…';
  let wav;
  try{wav=await blobToWav(raw);}
  catch(e){out.textContent='WAV encode error: '+e.message;return '';}
  out.textContent='Transcribing…';
  const form=new FormData();
  form.append('audio',wav,'audio.wav');
  try{
    const r=await fetch(`${API}/transcribe`,{method:'POST',body:form});
    const d=await r.json();
    out.textContent=JSON.stringify(d,null,2);
    return d.text||'';
  }catch(e){out.textContent='Error: '+e.message;return '';}
}

// ── Wake word ─────────────────────────────────────────────────────────────────
async function testWakeWord(){
  const btn=document.getElementById('wake-btn');
  const st=document.getElementById('wake-status');
  const out=document.getElementById('wake-out');
  btn.disabled=true;out.textContent='…';
  let secs=3;
  st.textContent=`Recording ${secs}s… say "Goose"`;
  const tick=setInterval(()=>{secs--;if(secs>0)st.textContent=`Recording ${secs}s… say "Goose"`;},1000);
  try{
    const stream=await navigator.mediaDevices.getUserMedia({audio:true});
    const wrec=new MediaRecorder(stream);
    const wchunks=[];
    wrec.ondataavailable=e=>wchunks.push(e.data);
    wrec.onstop=async()=>{
      clearInterval(tick);st.textContent='Converting…';
      stream.getTracks().forEach(t=>t.stop());
      const raw=new Blob(wchunks,{type:wrec.mimeType});
      let wav;
      try{wav=await blobToWav(raw);}
      catch(e){out.textContent='WAV encode error: '+e.message;st.textContent='';btn.disabled=false;return;}
      st.textContent='Transcribing…';
      const form=new FormData();form.append('audio',wav,'audio.wav');
      try{
        const r=await fetch(`${API}/transcribe`,{method:'POST',body:form});
        const d=await r.json();
        const text=(d.text||'').toLowerCase();
        const hit=text.includes('goose');
        out.textContent=`Transcript: "${d.text||'(empty)'}"\n\nWake word "goose": ${hit?'✅ DETECTED':'❌ not found'}`;
        st.innerHTML=hit?'<span class="detected">✅ Detected</span>':'<span class="not-detected">❌ Not detected</span>';
      }catch(e){out.textContent='Error: '+e.message;st.textContent='';}
      btn.disabled=false;
    };
    wrec.start();
    setTimeout(()=>wrec.stop(),3000);
  }catch(e){clearInterval(tick);out.textContent='Mic error: '+e.message;st.textContent='';btn.disabled=false;}
}

// ── Fallback provider ─────────────────────────────────────────────────────────
async function testFallback(){
  const out=document.getElementById('fallback-out');
  out.textContent='Testing…';
  const t0=Date.now();
  try{
    const r=await fetch(`${API}/chat`,{
      method:'POST',
      headers:{'Content-Type':'application/json','Authorization':'Bearer dev'},
      body:JSON.stringify({message:'Reply with exactly one word: pong'})
    });
    const d=await r.json();
    d._latency_ms=Date.now()-t0;
    out.textContent=JSON.stringify(d,null,2);
  }catch(e){out.textContent='Error: '+e.message;}
}

// ── TTS ───────────────────────────────────────────────────────────────────────
async function testTts(){
  const text=document.getElementById('tts-text').value.trim()||'Hello from Goose!';
  const out=document.getElementById('tts-out');
  out.textContent='Speaking…';
  try{
    const t0=Date.now();
    const r=await fetch(`${API}/test/speak`,{
      method:'POST',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({text})
    });
    const d=await r.json();
    d._client_latency_ms=Date.now()-t0;
    out.textContent=JSON.stringify(d,null,2);
  }catch(e){out.textContent='Error: '+e.message;}
}

// ── Weather ───────────────────────────────────────────────────────────────────
async function fetchWeather(){
  const badge=document.getElementById('wx-cfg-badge');
  const out=document.getElementById('wx-out');
  badge.className='badge pending';badge.textContent='…';
  out.style.display='none';
  try{
    const r=await fetch(`${API}/weather`);
    if(r.status===503){
      badge.className='badge unavailable';badge.textContent='disabled';
      document.getElementById('wx-display').style.display='none';
      out.style.display='block';out.textContent='Weather disabled — fill in lat/lon and click "Save & enable".';
      return;
    }
    const d=await r.json();
    if(d.error){
      badge.className='badge unavailable';badge.textContent='error';
      out.style.display='block';out.textContent=d.error;
      return;
    }
    badge.className='badge ok';badge.textContent='ok';
    renderWeather(d);
  }catch(e){
    badge.className='badge unavailable';badge.textContent='error';
    out.style.display='block';out.textContent='Error: '+e.message;
  }
}

async function testWeatherDirect(){
  const lat=parseFloat(document.getElementById('wx-lat').value);
  const lon=parseFloat(document.getElementById('wx-lon').value);
  const loc=document.getElementById('wx-loc').value.trim()||`${lat}, ${lon}`;
  const out=document.getElementById('wx-out');
  if(isNaN(lat)||isNaN(lon)){alert('Enter valid lat/lon first.');return;}
  out.style.display='block';out.textContent='Fetching from open-meteo.com…';
  document.getElementById('wx-display').style.display='none';
  try{
    const url=`https://api.open-meteo.com/v1/forecast?latitude=${lat}&longitude=${lon}`+
      `&current=temperature_2m,relative_humidity_2m,apparent_temperature,precipitation,`+
      `weather_code,wind_speed_10m,wind_direction_10m`+
      `&temperature_unit=celsius&wind_speed_unit=kmh&precipitation_unit=mm`;
    const r=await fetch(url);
    const api=await r.json();
    const c=api.current;
    const WMO={0:'Clear sky',1:'Mainly clear',2:'Partly cloudy',3:'Overcast',
      45:'Fog',48:'Fog',51:'Light drizzle',53:'Moderate drizzle',55:'Dense drizzle',
      61:'Slight rain',63:'Moderate rain',65:'Heavy rain',
      71:'Slight snow',73:'Moderate snow',75:'Heavy snow',
      80:'Slight showers',81:'Moderate showers',82:'Violent showers',
      95:'Thunderstorm',96:'Thunderstorm with hail',99:'Thunderstorm with hail'};
    const desc=WMO[c.weather_code]||'Unknown';
    const data={
      temperature_c:c.temperature_2m,feels_like_c:c.apparent_temperature,
      humidity_pct:c.relative_humidity_2m,description:desc,
      wind_speed_kmh:c.wind_speed_10m,wind_direction_deg:c.wind_direction_10m,
      precipitation_mm:c.precipitation,location_name:loc,
      fetched_at:new Date().toISOString()
    };
    renderWeather(data);
    out.textContent=JSON.stringify(api.current,null,2);
  }catch(e){out.textContent='Error: '+e.message;}
}

function renderWeather(d){
  document.getElementById('wx-display').style.display='block';
  document.getElementById('wx-temp').textContent=d.temperature_c.toFixed(1)+'°C';
  document.getElementById('wx-feels').textContent=d.feels_like_c.toFixed(1)+'°C';
  document.getElementById('wx-hum').textContent=d.humidity_pct+'%';
  document.getElementById('wx-wind').textContent=d.wind_speed_kmh.toFixed(0)+' km/h';
  document.getElementById('wx-precip').textContent=d.precipitation_mm.toFixed(1)+' mm';
  document.getElementById('wx-desc').textContent=`${d.description} — ${d.location_name}`;
  const block=`[Current Weather — ${d.location_name}]\n${d.description} | `+
    `${d.temperature_c.toFixed(1)}°C (feels like ${d.feels_like_c.toFixed(1)}°C) | `+
    `Humidity: ${d.humidity_pct}% | Wind: ${d.wind_speed_kmh.toFixed(0)} km/h | `+
    `Precip: ${d.precipitation_mm.toFixed(1)} mm`;
  document.getElementById('wx-context-block').textContent=block;
}

function geolocate(){
  if(!navigator.geolocation){alert('Geolocation not supported.');return;}
  navigator.geolocation.getCurrentPosition(pos=>{
    document.getElementById('wx-lat').value=pos.coords.latitude.toFixed(6);
    document.getElementById('wx-lon').value=pos.coords.longitude.toFixed(6);
  },err=>alert('Geolocation error: '+err.message));
}

async function saveWeatherSettings(){
  const lat=parseFloat(document.getElementById('wx-lat').value);
  const lon=parseFloat(document.getElementById('wx-lon').value);
  const loc=document.getElementById('wx-loc').value.trim();
  if(isNaN(lat)||isNaN(lon)){alert('Enter valid lat/lon first.');return;}
  const out=document.getElementById('wx-out');
  out.style.display='block';out.textContent='Saving…';
  try{
    const r=await fetch(`${API}/settings`,{
      method:'PUT',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({weather_enabled:true,weather_latitude:lat,weather_longitude:lon,weather_location_name:loc})
    });
    const d=await r.json();
    if(r.ok){out.textContent='Saved! Fetching weather…';fetchWeather();}
    else{out.textContent='Error: '+JSON.stringify(d);}
  }catch(e){out.textContent='Error: '+e.message;}
}

async function disableWeather(){
  const out=document.getElementById('wx-out');
  out.style.display='block';out.textContent='Disabling…';
  try{
    const r=await fetch(`${API}/settings`,{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({weather_enabled:false})});
    const d=await r.json();
    if(r.ok){out.textContent='Weather disabled.';fetchWeather();}
    else{out.textContent='Error: '+JSON.stringify(d);}
  }catch(e){out.textContent='Error: '+e.message;}
}

// ── Scheduler ─────────────────────────────────────────────────────────────────
async function listSchedules(){
  const out=document.getElementById('sched-out');
  const list=document.getElementById('sched-list');
  try{
    const r=await fetch(`${API}/schedules`);
    if(r.status===503){
      list.innerHTML='<p style="color:#f85149;font-size:0.82rem">Scheduler not configured (503).</p>';
      return;
    }
    const tasks=await r.json();
    if(!Array.isArray(tasks)||tasks.length===0){
      list.innerHTML='<p style="color:#8b949e;font-size:0.82rem">No scheduled tasks.</p>';
      return;
    }
    list.innerHTML=tasks.map(t=>`
      <div style="border:1px solid #30363d;border-radius:4px;padding:0.6rem 0.75rem;margin-bottom:0.5rem;display:flex;justify-content:space-between;align-items:center;gap:0.5rem;flex-wrap:wrap">
        <div style="flex:1;min-width:160px">
          <span style="font-weight:bold;font-size:0.88rem">${esc(t.id)}</span>
          <span style="color:#8b949e;font-size:0.78rem;margin-left:6px">${esc(t.label)}</span><br>
          <code style="font-size:0.75rem">${esc(t.cron)}</code>
          ${t.paused?'<span class="badge pending" style="margin-left:6px">paused</span>':''}
          ${t.currently_running?'<span class="badge ok" style="margin-left:6px">running</span>':''}
        </div>
        <div style="display:flex;gap:0.4rem;flex-wrap:wrap">
          <button class="secondary" style="padding:3px 8px;font-size:0.75rem" onclick="schedRunNow('${esc(t.id)}')">▶ Run now</button>
          ${t.paused
            ?`<button class="secondary" style="padding:3px 8px;font-size:0.75rem" onclick="schedResume('${esc(t.id)}')">Resume</button>`
            :`<button class="secondary" style="padding:3px 8px;font-size:0.75rem" onclick="schedPause('${esc(t.id)}')">Pause</button>`}
          <button class="danger" style="padding:3px 8px;font-size:0.75rem" onclick="schedDelete('${esc(t.id)}')">Delete</button>
        </div>
      </div>`).join('');
    out.style.display='none';
  }catch(e){
    list.innerHTML='';
    out.style.display='block';
    out.textContent='Error: '+e.message;
  }
}

function esc(s){return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}

async function schedAction(method,path,body){
  const out=document.getElementById('sched-out');
  out.style.display='block';
  out.textContent='…';
  try{
    const opts={method,headers:{'Content-Type':'application/json'}};
    if(body)opts.body=JSON.stringify(body);
    const r=await fetch(`${API}${path}`,opts);
    const d=await r.json();
    out.textContent=JSON.stringify(d,null,2);
    listSchedules();
  }catch(e){out.textContent='Error: '+e.message;}
}

async function createSchedule(){
  const id=document.getElementById('sched-id').value.trim();
  const label=document.getElementById('sched-label').value.trim()||id;
  const cron=document.getElementById('sched-cron').value.trim();
  const webhook=document.getElementById('sched-webhook').value.trim();
  if(!id||!cron){alert('ID and Cron are required.');return;}
  const payload=webhook?{webhook_url:webhook}:{};
  await schedAction('POST','/schedules',{id,label,cron,payload});
}
async function schedRunNow(id){await schedAction('POST',`/schedules/${id}/run-now`);}
async function schedPause(id){await schedAction('POST',`/schedules/${id}/pause`);}
async function schedResume(id){await schedAction('POST',`/schedules/${id}/resume`);}
async function schedDelete(id){
  if(!confirm(`Delete task "${id}"?`))return;
  await schedAction('DELETE',`/schedules/${id}`);
}

// ── Goose MCP Extensions ──────────────────────────────────────────────────────
async function loadGooseStatus(){
  const badge=document.getElementById('goose-badge');
  const list=document.getElementById('goose-ext-list');
  const out=document.getElementById('goose-out');
  badge.textContent='…';badge.className='badge pending';
  try{
    const r=await fetch(`${API}/dev/goose`);
    const d=await r.json();
    if(d.goose_active){
      badge.textContent='active';badge.className='badge ok';
      if(d.extensions&&d.extensions.length>0){
        list.innerHTML=d.extensions.map(e=>`
          <div style="border:1px solid #30363d;border-radius:4px;padding:0.5rem 0.75rem;margin-bottom:0.4rem;display:flex;justify-content:space-between;align-items:center">
            <div>
              <span style="font-weight:bold;font-size:0.88rem">${esc(e.name)}</span>
              <span class="badge unknown" style="margin-left:6px;font-size:0.72rem">${esc(e.kind)}</span>
              ${e.tools&&e.tools.length?'<br><span style="color:#8b949e;font-size:0.75rem">tools: '+e.tools.map(t=>esc(t)).join(', ')+'</span>':''}
            </div>
            <button class="danger" style="padding:3px 8px;font-size:0.75rem" onclick="removeExtension('${esc(e.name)}')">Remove</button>
          </div>`).join('');
      } else {
        list.innerHTML='<p style="color:#8b949e;font-size:0.82rem">No extensions loaded. Add the "giap" builtin to enable GIAP tools.</p>';
      }
      out.style.display='none';
    } else {
      badge.textContent='inactive';badge.className='badge unavailable';
      list.innerHTML=`<p style="color:#f85149;font-size:0.82rem">${esc(d.message||'Goose agent not active.')}</p>`;
      out.style.display='none';
    }
  }catch(e){badge.textContent='error';badge.className='badge unavailable';out.style.display='block';out.textContent='Error: '+e.message;}
}

function onExtKindChange(){
  const kind=document.getElementById('ext-kind').value;
  document.getElementById('ext-cmd-row').style.display=kind==='stdio'?'':'none';
  document.getElementById('ext-uri-row').style.display=kind==='streamable_http'?'':'none';
}

async function addExtension(){
  const out=document.getElementById('goose-out');
  const kind=document.getElementById('ext-kind').value;
  const name=document.getElementById('ext-name').value.trim();
  const desc=document.getElementById('ext-desc').value.trim();
  const cmd=document.getElementById('ext-cmd').value.trim();
  const uri=document.getElementById('ext-uri').value.trim();
  if(!name){alert('Name is required.');return;}
  const body={kind,name,description:desc,args:[],env:{}};
  if(kind==='stdio'){if(!cmd){alert('Command is required for stdio.');return;}body.command=cmd;}
  if(kind==='streamable_http'){if(!uri){alert('URI is required for http.');return;}body.uri=uri;}
  out.style.display='block';out.textContent='Adding…';
  try{
    const r=await fetch(`${API}/extensions`,{
      method:'POST',
      headers:{'Content-Type':'application/json','Authorization':'Bearer dev'},
      body:JSON.stringify(body)
    });
    const d=await r.json();
    out.textContent=JSON.stringify(d,null,2);
    loadGooseStatus();
  }catch(e){out.textContent='Error: '+e.message;}
}

async function removeExtension(name){
  if(!confirm(`Remove extension "${name}"?`))return;
  const out=document.getElementById('goose-out');
  out.style.display='block';out.textContent='Removing…';
  try{
    const r=await fetch(`${API}/extensions/${encodeURIComponent(name)}`,{
      method:'DELETE',
      headers:{'Authorization':'Bearer dev'}
    });
    out.textContent=r.ok?'Removed.':JSON.stringify(await r.json(),null,2);
    loadGooseStatus();
  }catch(e){out.textContent='Error: '+e.message;}
}

async function sendGooseChat(){
  const si=document.getElementById('goose-session');
  const msg=document.getElementById('goose-msg').value.trim();
  const out=document.getElementById('goose-chat-out');
  if(!msg)return;
  out.textContent='Thinking…';
  const body={message:msg};
  if(si.value.trim())body.session_id=si.value.trim();
  try{
    const r=await fetch(`${API}/chat`,{
      method:'POST',
      headers:{'Content-Type':'application/json','Authorization':'Bearer dev'},
      body:JSON.stringify(body)
    });
    const d=await r.json();
    if(d.session_id)si.value=d.session_id;
    out.textContent=JSON.stringify(d,null,2);
  }catch(e){out.textContent='Error: '+e.message;}
}

// ── Init ──────────────────────────────────────────────────────────────────────
checkServices();
fetchWeather();
listSchedules();
loadGooseStatus();
</script>
</body>
</html>"#;

/// `GET /dev/test` — self-contained HTML dev test panel.
///
/// Tests whisper, llamafile, ollama, fallback provider, wake word, and chat.
/// **Never expose this to the internet.**
pub async fn dev_test_page() -> Html<&'static str> {
    Html(DEV_TEST_HTML)
}

/// `GET /api/v1/dev/goose` — Goose agent status (public, dev only).
///
/// Returns whether the Goose agent is active and the extension manager is wired.
/// When active, also returns the current tool list.
async fn goose_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    match &state.extension_manager {
        None => Json(json!({
            "goose_active": false,
            "message": "Goose agent not active. Restart with: cargo run -p pond-server --features goose-agent -- serve --agent goose"
        })),
        Some(mgr) => {
            let tools = mgr.list_tools().await.unwrap_or_default();
            let extensions = mgr.list_extensions().await.unwrap_or_default();
            Json(json!({
                "goose_active": true,
                "extension_count": extensions.len(),
                "extensions": extensions.iter().map(|e| json!({
                    "name": e.name,
                    "kind": e.kind,
                    "tools": e.tools,
                })).collect::<Vec<_>>(),
                "tool_count": tools.len(),
                "tools": tools,
            }))
        }
    }
}

/// `POST /api/v1/test/speak`
///
/// Synthesise speech on the server device via the configured TTS engine.
/// Body: `{ "text": "hello world" }`
/// Response: `{ "status": "ok"|"unavailable", "engine": "piper"|"print"|"none", "text": "..." }`
async fn test_speak(
    State(state): State<Arc<AppState>>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Json<Value> {
    let text = match body {
        Ok(Json(v)) => v
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("Hello from Goose In A Pond!")
            .to_string(),
        Err(_) => "Hello from Goose In A Pond!".to_string(),
    };

    match &state.tts {
        Some(tts) => {
            let t0 = std::time::Instant::now();
            match tts.speak(&text).await {
                Ok(()) => Json(json!({
                    "status": "ok",
                    "text": text,
                    "latency_ms": t0.elapsed().as_millis() as u64,
                })),
                Err(e) => Json(json!({
                    "status": "error",
                    "text": text,
                    "error": e.to_string(),
                })),
            }
        }
        None => Json(json!({
            "status": "unavailable",
            "text": text,
            "message": "No TTS engine configured. Start server with --tts piper after running setup.",
        })),
    }
}

/// Hit `url` with a GET, return a status/latency object.
async fn probe(client: &reqwest::Client, url: &str, timeout_secs: u64) -> Value {
    let t0 = std::time::Instant::now();
    match client
        .get(url)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .send()
        .await
    {
        Ok(_) => json!({
            "status": "ok",
            "url": url,
            "latency_ms": t0.elapsed().as_millis() as u64,
        }),
        Err(e) => json!({
            "status": "unavailable",
            "url": url,
            "error": e.to_string(),
        }),
    }
}

// ───────────────────────── Scheduler Handlers ───────────────────────

/// `GET /api/v1/schedules` — list all scheduled tasks.
async fn list_schedules(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    match scheduler.list_tasks().await {
        Ok(tasks) => (StatusCode::OK, Json(json!(tasks))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

/// `POST /api/v1/schedules` — create a new scheduled task.
async fn create_schedule(
    State(state): State<Arc<AppState>>,
    result: Result<Json<CreateTaskRequest>, JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    let Json(req) = match result {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))),
    };
    match scheduler.create_task(req).await {
        Ok(task) => (StatusCode::CREATED, Json(json!(task))),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

/// `DELETE /api/v1/schedules/:id` — remove a scheduled task.
async fn delete_schedule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    match scheduler.delete_task(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"deleted": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

/// `POST /api/v1/schedules/:id/pause` — pause a scheduled task.
async fn pause_schedule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    match scheduler.pause_task(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"paused": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

/// `POST /api/v1/schedules/:id/resume` — resume a paused task.
async fn resume_schedule(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    match scheduler.resume_task(&id).await {
        Ok(()) => (StatusCode::OK, Json(json!({"resumed": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

/// `POST /api/v1/schedules/:id/run-now` — fire a task immediately.
async fn run_schedule_now(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(scheduler) = &state.scheduler else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "scheduler not configured"})),
        );
    };
    match scheduler.run_now(&id).await {
        Ok(()) => (StatusCode::ACCEPTED, Json(json!({"fired": id}))),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))),
    }
}

// ── Extension management handlers ─────────────────────────────────────────────

/// `GET /api/v1/extensions` — list all active Goose/MCP extensions.
async fn list_extensions_handler(
    State(state): State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Extension manager not available"})),
        )
            .into_response();
    };
    match manager.list_extensions().await {
        Ok(exts) => Json(json!({"extensions": exts})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `POST /api/v1/extensions` — register a new MCP extension and persist it.
async fn add_extension_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<pond_core::ports::extension_manager::AddExtensionRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Extension manager not available"})),
        )
            .into_response();
    };
    match manager.add_extension(req.clone()).await {
        Ok(info) => {
            // Persist so the server reconnects on restart.
            if let Some(repo) = &state.mcp_server_repo {
                let cfg = pond_core::ports::mcp_server::McpServerConfig {
                    id:          uuid::Uuid::new_v4().to_string(),
                    name:        req.name.clone(),
                    kind:        req.kind.clone(),
                    description: req.description.clone(),
                    command:     req.command.clone(),
                    args:        req.args.clone(),
                    env:         req.env.clone(),
                    uri:         req.uri.clone(),
                    enabled:     true,
                    created_at:  chrono::Utc::now().to_rfc3339(),
                };
                if let Err(e) = repo.save(&cfg).await {
                    tracing::warn!("Failed to persist MCP server '{}': {e}", req.name);
                }
            }
            (StatusCode::CREATED, Json(info)).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `DELETE /api/v1/extensions/:name` — remove a registered extension and its persisted config.
async fn remove_extension_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(manager) = &state.extension_manager else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Extension manager not available"})),
        )
            .into_response();
    };
    match manager.remove_extension(&name).await {
        Ok(()) => {
            if let Some(repo) = &state.mcp_server_repo {
                if let Err(e) = repo.delete(&name).await {
                    tracing::warn!("Failed to remove persisted MCP server '{name}': {e}");
                }
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
