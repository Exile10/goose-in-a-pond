//! Registers GIAP's modular MCP servers into Goose's builtin extension registry.
//!
//! Call `register_giap_extensions(...)` once at startup before creating any GooseAdapter.
//! The returned `Vec<String>` is the list of actually-registered extension names,
//! which depends on the `ext_*_enabled` settings toggles.

use anyhow::Result;
use goose::builtin_extension::register_builtin_extension;
use pond_adapters_weather::WeatherProvider;
use pond_core::mcp::ports::tools::tool_caller::ToolCaller;
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::device_control::DeviceControlPort;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::draft::DraftRepository;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::recipe::AgentRecipeRepository;
use pond_core::user_data::ports::scheduler::SchedulerPort;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use std::sync::{Arc, OnceLock};

/// Registered extension names — populated at startup by `register_giap_extensions()`.
/// Read by GooseAdapter to load/filter extensions per session.
static REGISTERED_EXTENSIONS: OnceLock<Vec<String>> = OnceLock::new();

/// Returns the list of builtin extensions that were registered at startup.
/// Panics if called before `register_giap_extensions()`.
pub fn registered_extensions() -> &'static [String] {
    REGISTERED_EXTENSIONS
        .get()
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

/// Register GIAP MCP servers as Goose builtin extensions, respecting settings toggles.
///
/// Must be called once at process startup before any GooseAdapter session.
/// Returns the list of extension names that were actually registered.
///
/// The `giap-draft` extension is always registered (safety feature — not toggleable).
/// Other extensions are gated by the corresponding `ext_*_enabled` settings.
pub fn register_giap_extensions(
    settings: &Settings,
    memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    scheduler: Option<Arc<dyn SchedulerPort>>,
    weather: Option<Arc<dyn WeatherProvider>>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
    recipe_repo: Arc<dyn AgentRecipeRepository + Send + Sync>,
    draft_repo: Arc<dyn DraftRepository + Send + Sync>,
    device_control: Arc<dyn DeviceControlPort + Send + Sync>,
    tool_caller: Option<Arc<dyn ToolCaller>>,
) -> Result<Vec<String>> {
    // Set the ToolCaller specialist — all MCP tools use it for param generation
    pond_mcp_server::set_tool_caller(tool_caller);

    let mut registered = Vec::new();

    // ── Always-on: draft server (safety feature) ────────────────────────────
    pond_mcp_server::init_draft_deps(draft_repo);
    register_builtin_extension("giap-draft", pond_mcp_server::spawn_draft_server);
    registered.push("giap-draft".into());

    // ── Always-on: toolkit server (Phase D2 escape hatch) ───────────────────
    // Not toggleable, for the same reason giap-draft is not: it is what makes
    // tool-relevance narrowing safe. With `tool_selection_mode = "relevant"` the
    // model sees a reduced set of extension schemas; these two tools are how it
    // discovers and loads a group nobody predicted. Its `ToolSelectionControl`
    // handle is installed separately (`init_toolkit_deps` in pond-server) because
    // the implementor is the agent adapter, which is built after this call — and
    // without the handle both tools truthfully report "everything is loaded".
    register_builtin_extension(
        pond_core::mcp::domain::tool_group::TOOLKIT_EXTENSION,
        pond_mcp_server::spawn_toolkit_server,
    );
    registered.push(pond_core::mcp::domain::tool_group::TOOLKIT_EXTENSION.into());

    // ── Toggleable extensions ───────────────────────────────────────────────

    if settings.ext_memory_enabled {
        pond_mcp_server::init_memory_deps(memory_repo, embedding_provider);
        register_builtin_extension("giap-memory", pond_mcp_server::spawn_memory_server);
        registered.push("giap-memory".into());
    }

    if settings.ext_schedule_enabled {
        if let Some(sched) = scheduler {
            pond_mcp_server::init_schedule_deps(sched, settings_repo.clone());
            register_builtin_extension("giap-schedule", pond_mcp_server::spawn_schedule_server);
            registered.push("giap-schedule".into());
        }
    }

    if settings.ext_weather_enabled {
        pond_mcp_server::init_weather_deps(weather);
        register_builtin_extension("giap-weather", pond_mcp_server::spawn_weather_server);
        registered.push("giap-weather".into());
    }

    if settings.ext_knowledge_enabled {
        pond_mcp_server::init_knowledge_deps(reqwest::Client::new());
        register_builtin_extension("giap-knowledge", pond_mcp_server::spawn_knowledge_server);
        registered.push("giap-knowledge".into());
    }

    if settings.ext_system_enabled {
        register_builtin_extension("giap-system", pond_mcp_server::spawn_system_server);
        registered.push("giap-system".into());
    }

    if settings.ext_device_enabled {
        pond_mcp_server::init_device_deps(
            device_registry.clone(),
            settings_repo.clone(),
            skill_repo,
            recipe_repo,
        );
        register_builtin_extension("giap-device", pond_mcp_server::spawn_device_server);
        registered.push("giap-device".into());

        // Device actuation surface (set_device_state) — grouped with device
        // read. The registry lets the tool resolve natural references
        // ("the light") to registered device ids.
        pond_mcp_server::init_device_control_deps(device_control, device_registry);
        register_builtin_extension(
            "giap-device-control",
            pond_mcp_server::spawn_device_control_server,
        );
        registered.push("giap-device-control".into());
    }

    // ── Knowledge expansion servers ────────────────────────────────────────
    // All share a single HTTP client pool for efficiency.
    let shared_http = pond_mcp_server::build_http_client();

    if settings.ext_news_enabled {
        // No settings repo: the Guardian and GNews keys live in the secret
        // store, installed by `init_secret_deps` in pond-server (PAI-2 P2).
        pond_mcp_server::init_news_deps(shared_http.clone());
        register_builtin_extension("giap-news", pond_mcp_server::spawn_news_server);
        registered.push("giap-news".into());
    }

    if settings.ext_finance_enabled {
        pond_mcp_server::init_finance_deps(shared_http.clone());
        register_builtin_extension("giap-finance", pond_mcp_server::spawn_finance_server);
        registered.push("giap-finance".into());
    }

    if settings.ext_discovery_enabled {
        pond_mcp_server::init_discovery_deps(shared_http, settings_repo);
        register_builtin_extension("giap-discovery", pond_mcp_server::spawn_discovery_server);
        registered.push("giap-discovery".into());
    }

    // Audit / privacy-audit server (#115). Its event-log handle is installed
    // separately via `init_audit_deps` in pond-server (where the logs DB is in
    // scope), so registration here only wires the spawn fn + toggle.
    if settings.ext_audit_enabled {
        register_builtin_extension("giap-audit", pond_mcp_server::spawn_audit_server);
        registered.push("giap-audit".into());
    }

    // Vision server (#130). Like giap-audit, its camera-event store handle is
    // installed separately via `init_vision_deps` in pond-server.
    if settings.ext_vision_enabled {
        register_builtin_extension("giap-vision", pond_mcp_server::spawn_vision_server);
        registered.push("giap-vision".into());
    }

    // Sensor data aggregator. Storage handle installed separately via
    // `init_sensor_deps` in pond-server (where the logs DB is in scope).
    if settings.ext_sensor_enabled {
        register_builtin_extension("giap-sensors", pond_mcp_server::spawn_sensor_server);
        registered.push("giap-sensors".into());
    }

    // Personal context (PAI-8 P2). Read-only: `search_context` and
    // `get_recent_context`, both scoped to whoever is speaking by the engine
    // session id in `_meta`. There is no `ingest_context` tool and there will
    // not be one -- the corpus is written by the ingest pipeline from sources
    // the household connected, and a tool that let the model write into it
    // would let a prompt injection plant a memo the assistant later quotes as
    // fact.
    //
    // OFF by default, for a reason the other toggles do not have. Registering
    // puts two tool schemas into EVERY turn's prompt, and until somebody
    // connects a source they can only ever answer "nothing found" -- so on the
    // target hardware, where tool schemas are already ~88% of a 4 096-token
    // window, this would be a per-turn cost forever for nothing. Its deps are
    // installed separately (`init_context_deps` in pond-server), like
    // giap-audit's and giap-vision's.
    if settings.ext_context_enabled {
        register_builtin_extension(
            "giap-context",
            pond_mcp_server::context::spawn_context_server,
        );
        registered.push("giap-context".into());
    }

    // Orchestration / delegation (PAI-6 P5). OFF by default -- and it was the
    // only `ext_*` toggle that was until PAI-8 P2, because turning it on means an autonomous
    // multi-turn agent running under `GooseMode::Auto`, which is not something
    // an install should acquire by upgrading. `Settings::default_ext_orchestrator_enabled`
    // returns false; reusing `default_ext_enabled` here would flip that.
    //
    // Like giap-audit and giap-vision, the handles it needs are installed
    // separately -- `init_orchestrator_deps` in pond-server. Unlike them, the
    // reason is ordering rather than scope: the orchestrator wraps the very
    // adapter this registration runs before.
    if settings.ext_orchestrator_enabled {
        register_builtin_extension(
            pond_core::mcp::domain::tool_group::ORCHESTRATOR_EXTENSION,
            pond_mcp_server::spawn_orchestrator_server,
        );
        registered.push(pond_core::mcp::domain::tool_group::ORCHESTRATOR_EXTENSION.into());
    }

    // Store for GooseAdapter to read
    let _ = REGISTERED_EXTENSIONS.set(registered.clone());

    tracing::info!(
        extensions = ?registered,
        "Registered {} builtin MCP modules", registered.len()
    );

    Ok(registered)
}
