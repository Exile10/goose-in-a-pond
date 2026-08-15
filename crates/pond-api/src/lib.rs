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
//! All clients are rate limited to 600 requests per 60 seconds (10 req/s burst).

pub mod cleanup;
pub mod middleware;
pub mod oauth_callback;
pub mod routes;
pub mod thought_filter;
pub mod tool_context;

/// Controls the lifecycle of the local llamafile server process.
///
/// Injected by `pond-server` so `pond-api` has no process-management logic.
/// `None` in tests — route handlers check `Option<Arc<dyn LlamafileManager>>`.
#[async_trait::async_trait]
pub trait LlamafileManager: Send + Sync {
    /// Ensure the llamafile server is running, starting it if necessary.
    ///
    /// `model_name` hints which model to start; the implementation resolves
    /// it from the model catalog if `None` or empty.
    ///
    /// Returns the base URL (`http://127.0.0.1:<port>`) the server is
    /// reachable at (even if startup is still in progress).
    async fn ensure_started(&self, model_name: Option<&str>) -> String;

    /// Returns `true` if the llamafile server is currently answering requests.
    async fn is_running(&self) -> bool;

    /// Ensure the llamafile server is running AND block until it is ready,
    /// up to `timeout_secs` seconds.
    ///
    /// Returns `(url, true)` when the server becomes ready within the timeout,
    /// or `(url, false)` if it did not become ready in time.
    /// `url` is always the base URL (`http://127.0.0.1:<port>`) regardless of outcome.
    async fn ensure_started_and_wait(
        &self,
        model_name: Option<&str>,
        timeout_secs: u64,
    ) -> (String, bool);
}

use axum::{middleware::Next, Router};
use pond_adapters_weather::WeatherProvider;
use pond_core::mcp::ports::extension_manager::ExtensionManagerPort;
use pond_core::mcp::ports::extension_marketplace::ExtensionMarketplace;
use pond_core::mcp::ports::mcp_knowledge::McpKnowledgePort;
use pond_core::mcp::ports::mcp_server::McpServerRepository;
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::models::ports::model_catalog_provider::ModelCatalogProvider;
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::models::ports::model_scheduler::ModelScheduler;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::security::ports::handshake::Handshake;
use pond_core::security::ports::policy::SecurityPolicy;
use pond_core::security::ports::telemetry::TelemetryPort;
use pond_core::shared::ports::event_bus::EventBus;
use pond_core::user_data::ports::camera_storage::CameraStorage;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::face_recognition::FaceRecognition;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;
use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
use pond_core::user_data::ports::recipe::AgentRecipeRepository;
use pond_core::user_data::ports::scheduler::SchedulerPort;
use pond_core::user_data::ports::sensor_storage::SensorStorage;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use pond_infra::db::Database;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

/// Callback that spawns the three-stage adversarial memory consolidation
/// pipeline. Injected by `pond-server` so `pond-api` has no dependency on
/// the consolidator implementation. Receives the cancellation token so the
/// caller can abort mid-run.
pub type ConsolidationRunner = Arc<
    dyn Fn(
            tokio_util::sync::CancellationToken,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Shared application state available to all route handlers.
pub struct AppState {
    pub db: Arc<Database>,
    pub handshake: Arc<dyn Handshake>,
    pub onboarding_repo: Arc<dyn OnboardingRepository + Send + Sync>,
    /// Base URL of the whisper.cpp server (e.g. "http://127.0.0.1:9000").
    pub whisper_url: String,
    /// In-process transcription callback. When `Some`, the `POST /api/v1/transcribe`
    /// handler uses this instead of proxying to the external whisper server.
    /// Receives raw WAV bytes, returns the transcript string.
    pub transcribe_audio: Option<Arc<dyn Fn(Vec<u8>) -> anyhow::Result<String> + Send + Sync>>,
    /// Session storage for conversation persistence.
    pub session_storage: Arc<dyn SessionStorage>,
    /// Shared HTTP client — reuse across requests to get connection pooling.
    pub http_client: reqwest::Client,
    /// Agent used as fallback when no LLM provider is configured.
    pub agent: Arc<dyn Agent>,
    /// LLM provider for AI-generated responses, wrapped in a RwLock so the
    /// ModelRouter can be hot-swapped when the user changes role assignments.
    /// `None` inside the lock → echo via agent.
    pub llm_provider: Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>,
    /// Base URL of the local llamafile server — stored here so the settings
    /// handler can rebuild the ModelRouter without restarting the server.
    pub llamafile_url: String,
    /// TTS engine for the `/api/v1/test/speak` dev endpoint. `None` → print only.
    pub tts: Option<Arc<dyn VoiceOutput>>,
    /// Persistent settings repository (assistant identity, LLM, voice, retention).
    pub settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    /// Profile repository for household members.
    pub profile_repo: Arc<dyn ProfileRepository + Send + Sync>,
    /// Device registry for GOTG devices and other connected hardware.
    pub device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    /// The Matter integration, reconciled at runtime from the `matter_enabled`
    /// setting. `None` only in builds and tests that wire no Matter support at
    /// all — "switched off" is a state of the runtime, not the absence of one,
    /// so that an unreachable controller can be reported as such instead of
    /// being mistaken for "Matter is not enabled".
    pub matter: Option<Arc<dyn pond_core::user_data::ports::matter_runtime::MatterRuntimePort>>,
    /// Memory fragment repository for semantic/recency-based retrieval.
    pub memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
    /// Embedding provider — `None` until a real embedding model is configured.
    /// Retained for Phase 3 memory vector search.
    pub embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    /// IoT sensor reading storage (uses logs DB).
    pub sensor_storage: Arc<dyn SensorStorage + Send + Sync>,
    /// Camera event storage (uses logs DB).
    pub camera_storage: Arc<dyn CameraStorage + Send + Sync>,
    /// Directory to look for user-supplied prompt overrides (e.g. `system.md`).
    /// Mirrors Goose's `~/.config/goose/prompts/` pattern.
    /// `None` in tests; `Some($DATA_DIR/prompts)` in production.
    pub prompt_template_dir: Option<std::path::PathBuf>,
    /// Persistent model catalog — replaces the old in-memory snapshot.
    /// `None` in tests that don't exercise model endpoints.
    pub model_repo: Option<Arc<dyn ModelRepository + Send + Sync>>,
    /// GIAP data directory — used by model endpoints to check file presence on disk.
    /// `None` in tests.
    pub data_dir: Option<std::path::PathBuf>,
    /// Skip onboarding check in middleware. Set to `true` in tests.
    pub skip_onboarding: bool,
    /// Cron-based task scheduler. `None` until `pond-infra-scheduler` is wired in.
    pub scheduler: Option<Arc<dyn SchedulerPort>>,
    /// Memory-aware model scheduler. `None` when all roles use external providers.
    pub model_scheduler: Option<Arc<dyn ModelScheduler>>,
    /// MCP-style persistent memory. `None` until `pond-adapters-mcp-memory` is wired in.
    pub mcp_memory: Option<Arc<dyn McpKnowledgePort + Send + Sync>>,
    /// MCP extension manager — manages Goose extensions for tool calling.
    /// `None` until a Goose agent with extension support is wired in.
    pub extension_manager: Option<Arc<dyn ExtensionManagerPort>>,
    /// Persistent storage for configured external MCP server connections.
    /// Loaded at startup to auto-connect saved servers.
    pub mcp_server_repo: Option<Arc<dyn McpServerRepository>>,
    /// Dynamic tool registry — unified view of all built-in + extension tools.
    /// Used by route handlers to sync the registry when extensions are added/removed.
    pub tool_registry:
        Option<Arc<dyn pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort>>,
    /// Direct MCP tool dispatcher — used by `POST /api/v1/tools/invoke` to run a
    /// tool by name without the LLM. `None` in tests / when unavailable.
    pub tool_dispatcher:
        Option<Arc<dyn pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher>>,
    /// Extension marketplace — curated registry of installable MCP extensions.
    pub marketplace: Option<Arc<dyn ExtensionMarketplace>>,
    /// Secure secret storage for extension API keys and OAuth tokens.
    pub secret_repo:
        Option<Arc<dyn pond_core::security::ports::secret::SecretRepository + Send + Sync>>,

    /// Tracks in-progress model downloads so the UI can show progress bars.
    pub download_tracker:
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, DownloadEntry>>>,
    /// Port the Piper HTTP TTS server is listening on.
    /// Set at startup by `piper_http::start()`. `None` if Piper is not running.
    pub piper_http_port: Option<u16>,
    /// Catalog provider — fetches the online model registry and returns typed records.
    /// Injected by pond-server so pond-api has no HTTP or parsing logic.
    /// `None` in tests.
    pub model_catalog_provider: Option<Arc<dyn ModelCatalogProvider>>,
    /// Filesystem storage helper — resolves on-disk paths for model records.
    /// Used by the refresh handler to update `downloaded` flags.
    /// `None` in tests.
    pub model_storage_dir: Option<std::path::PathBuf>,
    /// System prompt templates — editable by user, seeded from defaults at setup.
    /// `None` in tests that don't exercise prompt template endpoints.
    pub prompt_template_repo: Option<Arc<dyn PromptTemplateRepository + Send + Sync>>,
    /// Per-key extra instructions injected into the system prompt each turn.
    /// `None` in tests that don't exercise prompt extra endpoints.
    pub prompt_extra_repo: Option<Arc<dyn PromptExtraRepository + Send + Sync>>,
    /// User-defined skills injected as named system prompt extras.
    /// `None` in tests that don't exercise skill endpoints.
    pub skill_repo: Option<Arc<dyn UserSkillRepository + Send + Sync>>,
    /// Agent recipes (Goose Recipe YAML definitions).
    /// `None` in tests that don't exercise recipe endpoints.
    pub recipe_repo: Option<Arc<dyn AgentRecipeRepository + Send + Sync>>,
    /// Controls the llamafile server lifecycle.
    /// Set by pond-server when `chat_provider` may be "llamafile".
    /// `None` in tests and when the llamafile backend is not available.
    pub llamafile_manager: Option<Arc<dyn LlamafileManager>>,
    /// Read/write access to the `event_log` table in `pond_logs.db`.
    /// Used by the `/api/v1/logs` endpoint.
    /// `None` in tests.
    pub operational_log:
        Option<Arc<dyn pond_core::security::ports::event_log::OperationalLogRepository>>,
    /// In-process event bus (#91): `record_sensor` / `record_camera_event`
    /// publish here so reactive consumers (the rules engine, live dashboards)
    /// can respond. `None` in tests that don't exercise the bus.
    pub event_bus: Option<Arc<dyn EventBus>>,
    /// Unified, append-only event store (#109): the durable `events` table that
    /// the bus→log bridge writes to. Read by the activity query API (#114).
    /// `None` in tests that don't exercise it.
    pub event_log: Option<Arc<dyn pond_core::security::ports::event_log::EventLog>>,
    /// Push-notification token store (#95): a paired GOTG device's current
    /// FCM/APNs/Expo token. `None` when push storage isn't wired (tests) — the
    /// push-token endpoints then return 503.
    pub push_token_repo:
        Option<Arc<dyn pond_core::user_data::ports::push_token::PushTokenRepository>>,
    /// Biometric face recognition service (register + identify household
    /// members from camera frames).  `None` when no ONNX embedding model
    /// is configured — all face endpoints then return 503.
    pub face_recognition: Option<Arc<dyn FaceRecognition>>,
    /// Limits concurrent SSE streams to prevent unbounded memory use from
    /// stalled or abandoned clients. Acquired at the start of `chat_stream`
    /// and `agent_chat_stream`; dropped when the stream ends or disconnects.
    pub sse_semaphore: Arc<tokio::sync::Semaphore>,
    /// Bounds concurrent `/notifications/stream` connections (#99). Kept
    /// SEPARATE from `sse_semaphore`: a phone holds its notification stream
    /// open indefinitely, so sharing the small interactive-chat pool would let
    /// a few connected devices starve chat streaming entirely.
    pub notification_sse_semaphore: Arc<tokio::sync::Semaphore>,
    /// Answer Reviewer — adversarial post-inference review that evaluates
    /// answer quality and triggers revision when below threshold.
    pub answer_reviewer: Option<Arc<dyn pond_core::models::ports::answer_reviewer::AnswerReviewer>>,
    /// Memory Extractor — extracts durable facts from conversation turns.
    /// `None` when `memory_extraction_enabled` is false.
    pub memory_extractor:
        Option<Arc<dyn pond_core::user_data::ports::memory_extractor::MemoryExtractor>>,
    /// Shared extraction service instance (rate limiter + dedup state).
    pub memory_extraction_service:
        Option<Arc<pond_core::user_data::services::memory_extraction::MemoryExtractionService>>,
    /// Timestamp of the last user request — used by the inactivity-based
    /// consolidation scheduler. Updated on every chat/API call.
    pub last_user_activity: Arc<tokio::sync::RwLock<std::time::Instant>>,
    /// Cancellation token for in-progress consolidation. When a user request
    /// arrives, this token is cancelled to stop consolidation immediately.
    pub consolidation_cancel: Arc<tokio::sync::RwLock<Option<tokio_util::sync::CancellationToken>>>,
    /// Broadcast channel for consolidation events (streamed to SSE for the UI modal).
    pub consolidation_event_tx: tokio::sync::broadcast::Sender<
        pond_core::user_data::ports::memory_consolidator::ConsolidationEvent,
    >,
    /// Injected by `pond-server` — spawns the three-stage adversarial
    /// consolidation pipeline.  `pond-api` never imports the consolidator
    /// directly; the closure captures everything it needs.
    /// `None` when `memory_consolidation_enabled` is false.
    pub consolidation_runner: Option<ConsolidationRunner>,
    /// Inference pool — concurrent LLM task submission with provider-aware
    /// semaphore (3 for HTTP providers, 1 for GGUF). Used for parallel
    /// post-processing (review + extraction can run concurrently on HTTP providers).
    pub inference_pool: Option<Arc<dyn pond_core::models::ports::inference_pool::InferencePool>>,
    /// Broadcast channel for schedule completion events (SSE + desktop notifications).
    pub schedule_result_tx:
        tokio::sync::broadcast::Sender<pond_core::user_data::domain::schedule::ScheduleResultEvent>,
    /// Push-notification fan-out (#99): connected `/notifications/stream` clients
    /// subscribe to this; producers send via `notification_sender`.
    pub notification_tx:
        tokio::sync::broadcast::Sender<pond_core::mcp::ports::notification::Notification>,
    /// Offline notification queue (#99) — read on stream connect to flush
    /// notifications that arrived while a device was disconnected. `None` in tests.
    pub notification_queue:
        Option<Arc<dyn pond_core::mcp::ports::notification_queue::NotificationQueueRepository>>,
    /// Notification sender port (#99): producers (the `send_notification` tool,
    /// the schedule bridge) route through this. `None` in tests.
    pub notification_sender:
        Option<Arc<dyn pond_core::mcp::ports::notification::NotificationSender>>,
    /// Per-turn telemetry recorder. `None` when `telemetry_enabled` is false.
    pub telemetry: Option<Arc<dyn TelemetryPort>>,
    /// Context growth monitor — tracks context window fill rate per session
    /// and emits warnings before the "context cliff" where quality degrades.
    pub context_monitor: Arc<pond_core::models::services::context_monitor::ContextMonitor>,
    /// Static registry of MCP App resources: maps `ui://` URIs to embedded HTML
    /// content. Populated at startup from `pond_mcp_server::all_app_resources()`.
    /// Used by `GET /api/v1/mcp/resources?uri=...` to serve MCP App HTML.
    pub mcp_app_resources: std::collections::HashMap<String, &'static str>,
    /// In-memory OAuth PKCE sessions (state nonce -> verifier + provider).
    pub oauth_state: crate::oauth_callback::OAuthState,
    /// Outcomes of finished OAuth flows (state nonce -> completed / failed),
    /// so the UI that started a flow can tell whether it actually succeeded
    /// rather than inferring it from the token key's existence.
    pub oauth_outcomes: crate::oauth_callback::OAuthOutcomes,
    /// Authorization + audit hook at the privacy/security boundary.
    ///
    /// A hook, not a gate: the default implementation allows everything and
    /// only records audits. Routes opt in by calling `allow`/`audit`; until a
    /// route does, behaviour is unchanged. `None` in tests.
    pub security_policy: Option<Arc<dyn SecurityPolicy>>,
    /// The port the API server is actually listening on.
    /// Used to construct OAuth redirect URIs dynamically (the server may bind
    /// to a port other than 4000 if that port is already in use).
    pub api_port: u16,
    /// Weather provider (Open-Meteo) for `GET /api/v1/weather`. `None` when
    /// `weather_enabled` is false or no location has been configured.
    pub weather_provider: Option<Arc<dyn WeatherProvider>>,
    /// Private mesh (#132): who this Pond trusts and at what scope. Plain
    /// SQLite, no extra dependency — always constructed, unlike
    /// `mesh_transport` below.
    pub peer_directory:
        Arc<dyn pond_core::mesh::ports::peer_directory::PeerDirectory + Send + Sync>,
    /// Private mesh (#132): prepaid-credit balance held with each trusted peer.
    pub credit_ledger: Arc<dyn pond_core::mesh::ports::credit_ledger::CreditLedger + Send + Sync>,
    /// Private mesh (#132): metered token usage pending settlement per peer.
    pub usage_tally: Arc<dyn pond_core::mesh::ports::usage_tally::UsageTally + Send + Sync>,
    /// Private mesh (#132): real libp2p connectivity to trusted peers. `None`
    /// unless both the `mesh` Cargo feature is compiled in and
    /// `settings.mesh_enabled` is true — real networking, real cost, unlike
    /// the three ports above.
    pub mesh_transport: Option<Arc<dyn pond_core::mesh::ports::mesh_transport::MeshTransport>>,
}

impl AppState {
    /// Record that the user just did something, and get out of the way.
    ///
    /// Every route that starts real work on the user's behalf must call this
    /// **first**. It does two things that must always happen together:
    ///
    /// 1. Resets the inactivity clock the consolidation scheduler reads, so a
    ///    background pass is not started while the user is mid-interaction.
    /// 2. Cancels any consolidation already in flight. On the Jetson there is a
    ///    single inference slot; a running three-stage pass would otherwise sit
    ///    in front of the user's turn.
    ///
    /// Exists as one helper precisely so a new route cannot half-remember the
    /// pair — the two blocks used to be copy-pasted into each handler, and the
    /// voice and plain-`/chat` paths were missing them.
    ///
    /// Note: the terminal voice loop runs in a *separate process* and never
    /// reaches this. It is covered instead by the scheduler's second activity
    /// source, the newest `sessions.updated_at` in `pond_system.db`.
    pub async fn note_user_activity(&self) {
        *self.last_user_activity.write().await = std::time::Instant::now();
        if let Some(cancel) = self.consolidation_cancel.read().await.as_ref() {
            cancel.cancel();
        }
    }
}

/// Keep going.
pub const DL_RUN: u8 = 0;
/// Stop, but leave the partial file so it can be picked up again.
pub const DL_PAUSE: u8 = 1;
/// Stop and throw the partial file away.
pub const DL_CANCEL: u8 = 2;

/// State of a single in-progress (or recently completed) model download.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadEntry {
    pub filename: String,
    pub category: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    /// "downloading" | "paused" | "done" | "error" | "cancelled"
    pub status: String,
    /// When the download finished (status became "done" or "error").
    /// `None` while still downloading. Used to evict stale entries.
    #[serde(skip)]
    pub finished_at: Option<std::time::Instant>,
    /// What this download has been told to do — [`DL_RUN`], [`DL_PAUSE`] or
    /// [`DL_CANCEL`].
    ///
    /// Shared with the transfer, which reads it between chunks. That is the
    /// only place a download that is already streaming can be stopped: the
    /// task is inside `resp.chunk().await` the rest of the time, and nothing
    /// outside it can interrupt that without dropping the connection.
    #[serde(skip)]
    pub control: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// The source, so a paused transfer can be asked for again. Hugging Face
    /// downloads resume from a `.incomplete` file, so re-requesting is all a
    /// resume needs.
    #[serde(skip)]
    pub url: Option<String>,
}

/// Snapshot of one model's availability, sent over the REST API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStatusEntry {
    pub category: String,
    pub name: String,
    pub description: String,
    pub size_mb: u64,
    /// True if the model file exists on disk (or for HTTP TTS, always true).
    pub downloaded: bool,
    /// True if this is the currently active model for its category.
    pub active: bool,
    /// Download URL — None for HTTP TTS entries that have no downloadable file.
    pub url: Option<String>,
    /// HuggingFace model spec (GGUF only): "author/repo:quantization"
    pub hf_id: Option<String>,
    /// Filename on disk (used by the download route to determine the save path)
    pub filename: Option<String>,
    /// Approximate RAM required at runtime in MB. None for models without estimates.
    pub ram_estimate_mb: Option<u64>,
    /// Suggested role assignment: "chat" | "think" | "task". None = general purpose.
    pub recommended_role: Option<String>,
    /// Maximum context tokens this model declares. LLM rows only; `None` for
    /// ASR/TTS/embedding, and for LLM rows whose catalog provider could not
    /// answer.
    ///
    /// This is rung 3 of the context governor
    /// (`docs/architecture/pai/03-context-governor.md`), surfaced so the Models
    /// UI stops inferring the window from the model's NAME — a third copy of a
    /// heuristic the backend already replaced with a real number.
    pub context_length: Option<u32>,
    /// ASR language ("en", "multilingual"). Whisper models only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asr_language: Option<String>,
    /// ASR model size ("tiny", "base", "small", …). Whisper models only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asr_size: Option<String>,
    /// TTS engine identifier ("piper"). TTS models only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_engine: Option<String>,
    /// Companion config filename (.onnx.json). TTS models only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_filename: Option<String>,
}

/// Build the full API router.
/// True when the web UI is embedded into this binary (a single-executable build,
/// i.e. `pond-desktop/dist` was present when the binary was compiled).
pub fn web_ui_embedded() -> bool {
    routes::embedded_ui_present()
}

///
/// Web dashboard: `/{route_name}`
/// REST API:      `/api/v1/{route_name}`
pub fn build_router(state: Arc<AppState>, static_dir: std::path::PathBuf) -> Router {
    // Rate limiter for remote clients (GOTG app, external integrations).
    // 600 req/60s = 10 req/s burst — generous for API use, still protects against abuse.
    // Loopback clients (local web dashboard) are exempted entirely in the middleware.
    let rate_limiter = Arc::new(middleware::RateLimiter::new(
        600,
        std::time::Duration::from_secs(60),
    ));
    // Pairing gets its own, separate budget. Sharing one bucket with ordinary
    // API traffic meant a single chatty or wedged client could spend the whole
    // per-IP allowance and lock the device out of `/handshake` — and re-pairing
    // is precisely the recovery path you reach for when a client is
    // misbehaving. Keeping the budgets independent means API abuse can never
    // take pairing down with it. Pairing is inherently low-volume, so a much
    // smaller allowance is ample; `/handshake/verify` keeps its own stricter
    // limiter on top of this (see `routes::verify_limiter`).
    let handshake_limiter = Arc::new(middleware::RateLimiter::new(
        30,
        std::time::Duration::from_secs(60),
    ));

    Router::new()
        // Dev test page — no auth required, returns HTML
        .route("/dev/test", axum::routing::get(routes::dev_test_page))
        .route("/dev/face", axum::routing::get(routes::dev_face_page))
        .nest("/api/v1", routes::api_routes(state.clone()))
        // Serve the web UI: embedded-into-the-binary (single executable) when the
        // UI was built in, otherwise from the on-disk `static_dir` (dev).
        .fallback(move |uri: axum::http::Uri| routes::serve_web(uri, static_dir.clone()))
        // Log every request/response at DEBUG level.
        .layer(axum::middleware::from_fn(middleware::log_requests))
        // Enforce Bearer token authentication on all protected routes.
        // Must come AFTER log_requests (layers apply in reverse order in axum).
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ))
        // Apply rate limiting to all routes
        .layer(axum::middleware::from_fn(move |req, next| {
            let limiter = rate_limiter.clone();
            let handshake_limiter = handshake_limiter.clone();
            rate_limit_with_limiter(req, next, limiter, handshake_limiter)
        }))
        // CORS — scoped to the first-party Tauri desktop origins (#94). Browser
        // requests from other origins are rejected. Native GOTG mobile clients
        // don't send a browser `Origin` header, so they're unaffected. Operators
        // can allow-list extra origins via `POND_CORS_ALLOWED_ORIGINS`
        // (comma-separated, e.g. a LAN dashboard URL).
        .layer(build_cors_layer())
        .with_state(state)
}

/// Build the CORS layer with a scoped origin allowlist (see call site).
fn build_cors_layer() -> CorsLayer {
    use axum::http::{header, HeaderValue, Method};

    let mut origins: Vec<HeaderValue> = [
        "tauri://localhost",
        "http://localhost:1420",
        "http://127.0.0.1:1420",
    ]
    .iter()
    .filter_map(|o| o.parse().ok())
    .collect();
    if let Ok(extra) = std::env::var("POND_CORS_ALLOWED_ORIGINS") {
        for o in extra.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if let Ok(v) = o.parse() {
                origins.push(v);
            }
        }
    }

    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

/// True for the pairing endpoints, which draw on their own rate-limit budget.
/// Matched on a segment boundary so a merely similar path — `/handshakes` —
/// cannot help itself to the protected pairing allowance.
fn is_handshake_path(path: &str) -> bool {
    match path.strip_prefix("/api/v1/handshake") {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

async fn rate_limit_with_limiter(
    req: axum::extract::Request,
    next: Next,
    limiter: Arc<middleware::RateLimiter>,
    handshake_limiter: Arc<middleware::RateLimiter>,
) -> Result<axum::response::Response, middleware::AuthError> {
    // Extract client IP from ConnectInfo<SocketAddr> (populated by
    // into_make_service_with_connect_info in main.rs).
    let connect_info = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0);

    let client_ip = connect_info
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Loopback clients are the local web dashboard — never rate limit them.
    // Rate limiting only applies to remote clients (GOTG app, external integrations).
    if connect_info.map(|a| a.ip().is_loopback()).unwrap_or(false) {
        return Ok(next.run(req).await);
    }

    let limiter = if is_handshake_path(req.uri().path()) {
        &handshake_limiter
    } else {
        &limiter
    };
    if let Err(remaining) = limiter.check_rate_limit_detailed(&client_ip).await {
        return Err(middleware::AuthError::RateLimitExceeded {
            retry_after_secs: middleware::retry_after_secs(remaining),
        });
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod rate_limit_tests {
    use super::*;

    /// Every pairing endpoint must draw on the pairing budget. If one of these
    /// fell back to the shared pool, a chatty client could still lock the
    /// device out of the recovery path it is routed here to protect.
    #[test]
    fn every_handshake_route_uses_the_pairing_budget() {
        for path in [
            "/api/v1/handshake",
            "/api/v1/handshake/init",
            "/api/v1/handshake/verify",
            "/api/v1/handshake/refresh",
            "/api/v1/handshake/revoke",
            "/api/v1/handshake/pairing-code",
        ] {
            assert!(is_handshake_path(path), "{path} should use it");
        }
    }

    /// And nothing else may draw on it — least of all the push-token endpoint,
    /// whose retry loop is what motivated separating the two budgets.
    #[test]
    fn ordinary_api_routes_do_not_use_the_pairing_budget() {
        for path in [
            "/api/v1/devices/abc/push-token",
            "/api/v1/chat/stream",
            "/api/v1/notifications/stream",
            "/api/v1/activity",
            "/api/v1/settings",
            // Near-misses: neither is a pairing route.
            "/api/v1/handshakes",
            "/api/v1/device-handshake",
        ] {
            assert!(!is_handshake_path(path), "{path} should not use it");
        }
    }

    /// Exhausting one budget must leave the other untouched — the whole point
    /// of keeping them separate.
    #[tokio::test]
    async fn exhausting_the_api_budget_leaves_pairing_available() {
        let api = middleware::RateLimiter::new(2, std::time::Duration::from_secs(60));
        let handshake = middleware::RateLimiter::new(2, std::time::Duration::from_secs(60));

        let ip = "192.0.2.10";
        while api.check_rate_limit(ip).await {}
        assert!(!api.check_rate_limit(ip).await, "API budget is spent");

        assert!(
            handshake.check_rate_limit(ip).await,
            "the same IP can still pair"
        );
    }
}
