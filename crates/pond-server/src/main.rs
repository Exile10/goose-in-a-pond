//! Goose In A Pond — Server Entry Point
//!
//! Usage:
//!   pond-server setup [--model tiny|base|small]
//!   pond-server serve [--port PORT] [--open]
//!   pond-server chat  [--provider mock|llamafile|ollama] [--model MODEL] [--input stdin|whisper] [--whisper-url URL]
//!   pond-server status
//!
//! # TODO — Setup Script
//! - [ ] Create a setup script (`scripts/setup.sh`) that:
//!   1. Detects whether the device is dedicated (sole GIAP) or shared
//!   2. If dedicated: configures `http://pond.local/{route}` (port 80)
//!   3. If shared:    configures `http://pond.<HOSTNAME>.local:<PORT>/{route}`
//!   4. Preferred port order: 80 → 8080 → 4000 → 5000
//!   5. Sets up mDNS/Avahi for `.local` hostname resolution
//!   6. Creates systemd service for auto-start on boot
//!   7. Initializes databases at a configurable data directory
//!   8. Prompts for initial onboarding if not yet done

mod llamafile_process;
mod model_download;
mod model_registry;
mod piper_http;
mod piper_process;
mod ports;
mod qwen_tts_process;
mod system_deps;
mod whisper_process;

use anyhow::Result;
use clap::{Parser, Subcommand};
use pond_adapters_llamafile::LlamafileProvider;
use pond_adapters_ollama::OllamaProvider;
use pond_adapters_piper::PiperOutput;
use pond_adapters_qwen_tts::QwenTtsOutput;
use pond_adapters_whisper::{WhisperInput, WhisperKeywordDetector};
use pond_core::ports::wake_word::WakeWordDetector;
use pond_core::ports::voice_input::VoiceInput;
use pond_core::ports::voice_output::VoiceOutput;
use pond_core::services::instant_activation::InstantActivation;
use pond_core::services::print_output::PrintOutput;
use pond_api::AppState;
use pond_core::ports::agent::Agent;
use pond_core::ports::provider::LlmProvider;
use pond_core::ports::session_storage::SessionStorage;
use pond_core::ports::mcp_server::McpServerRepository as _;
use pond_core::ports::settings::SettingsRepository as _;
use pond_core::prompts::build_system_prompt;
use pond_core::services::chat::ChatService;
use pond_core::services::model_router::ModelRouter;
use pond_core::services::fallback_voice_output::FallbackVoiceOutput;
use pond_core::services::mock_agent::MockAgent;
use pond_core::services::stdin_input::StdinInput;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra_scheduler::{CronSchedulerAdapter, WebhookTaskExecutor};
use pond_adapters_weather::{OpenMeteoWeatherAdapter, WeatherProvider};
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_memory::SqliteMemoryRepository;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_sensor::{SqliteCameraStorage, SqliteSensorStorage};
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_mcp_servers::SqliteMcpServerRepository;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
use std::sync::Arc;
use pond_core::services::onboarding::OnboardingService;
use pond_core::domain::onboarding::OnboardingStep;
use pond_infra::onboarding::SqlxOnboardingRepository;
use std::io::{self, Write};
use std::collections::HashMap;
use std::net::UdpSocket;

#[derive(Parser)]
#[command(name = "pond")]
#[command(about = "🦆 Goose In A Pond — Local AI Home Assistant")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// First-time setup: initialize databases and download the Whisper ASR model
    Setup {
        /// Whisper model size: tiny (~39 MB), base (~74 MB, default), small (~244 MB)
        #[arg(long, default_value = "base")]
        model: String,
    },

    /// Start the HTTP server (REST API + web dashboard)
    Serve {
        /// Path to the built web dashboard assets (run `cd web && npm run build` first)
        #[arg(long, default_value = "web/dist")]
        static_dir: std::path::PathBuf,

        /// Open the dashboard in the browser
        #[arg(long)]
        open: bool,

        /// Enable debug logging
        #[arg(long)]
        debug: bool,

        /// Agent backend: goose (default, Block's Goose with MCP tool calls) or mock (fast, no LLM).
        /// Override: cargo run -p pond-server -- serve --agent mock
        #[arg(long, default_value = "goose")]
        agent: String,
    },

    /// Interactive CLI chat (Wait→Listen→Think→Speak loop)
    Chat {
        /// LLM provider: mock, llamafile, ollama, or local (GGUF in-process, requires --features local-inference).
        /// Defaults to the value stored in Settings (llm_provider field).
        #[arg(short = 'P', long)]
        provider: Option<String>,

        /// Model name (only used when --provider ollama, e.g. "llama3.2", "gemma2")
        #[arg(short = 'M', long)]
        model: Option<String>,

        /// Input source: stdin (text) or whisper (microphone → ASR)
        #[arg(short = 'I', long, default_value = "stdin")]
        input: String,

        /// Enable voice-based wake word detection (requires --input whisper).
        /// Say the trigger phrase to activate the assistant before each turn.
        #[arg(long, default_value = "goose")]
        wake_word: Option<String>,

        /// Disable wake word detection (jump straight to listen on each turn).
        #[arg(long)]
        no_wake_word: bool,

        /// Text-to-speech engine: qwen (default), piper, or none (print only)
        #[arg(long, default_value = "qwen")]
        tts: String,

        /// Path to the Piper voice model (.onnx file). Defaults to $DATA_DIR/models/tts/en_US-lessac-medium.onnx
        #[arg(long)]
        tts_model: Option<std::path::PathBuf>,
    },

    /// Show system status
    Status,

    /// Run interactive onboarding wizard
    Onboard {
        /// Reset and restart onboarding from scratch
        #[arg(long)]
        reset: bool,
    },
}


#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Setup { model }) => {
            run_setup(&model).await
        }
        Some(Commands::Serve { static_dir, open, debug, agent }) => {
            init_tracing(debug);
            run_server(static_dir, open, debug, &agent).await
        }
        Some(Commands::Chat { provider, model, input, wake_word, no_wake_word, tts, tts_model }) => {
            init_tracing(false);
            run_chat(provider.as_deref(), model.as_deref(), &input, wake_word.as_deref(), no_wake_word, &tts, tts_model).await
        }
        Some(Commands::Status) => {
            run_status().await
        }
        Some(Commands::Onboard { reset }) => {
            if let Err(err) = run_onboard(reset).await {
                eprintln!("Error: {:?}", err);
            }
            Ok(())
        }
        None => {
            // Default: run interactive chat (backward compat) — provider comes from Settings
            init_tracing(false);
            run_chat(None, None, "stdin", None, true, "none", None).await
        }
    }
}

fn init_tracing(debug: bool) {
    // In debug mode, our own crates run at DEBUG while noisy third-party crates
    // (sqlx, hyper, tower, reqwest) are capped at WARN so their internal query
    // and connection tracing does not drown out the useful output.
    //
    // RUST_LOG always takes priority, so a developer can still override any
    // target at runtime:
    //   RUST_LOG=sqlx=debug cargo run -p pond-server -- serve --debug
    let filter = if debug {
        "debug,sqlx=warn,hyper=warn,tower=warn,reqwest=warn,hyper_util=warn,rustls=warn"
    } else {
        "info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| filter.into()),
        )
        .init();
}

async fn run_setup(model: &str) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose In A Pond — Setup         ║");
    println!("  ╚═══════════════════════════════════════╝");

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("goose-in-a-pond");

    println!("\n  📂 Data directory: {}", data_dir.display());

    // Step 1: Check + auto-install system dependencies (Linux/macOS only)
    println!("\n  [1/7] Checking system dependencies...");
    if system_deps::ensure_system_deps().await {
        println!("  ✅ System dependencies OK");
    } else {
        println!("  ⚠  Some system deps could not be installed — see docs/developer/linux-setup.md");
        println!("     Continuing setup; some features may not work until deps are installed.");
    }

    // Step 2: Initialize databases
    println!("\n  [2/7] Initializing databases...");
    Database::init(&data_dir).await?;
    println!("  ✅ Databases ready");

    // Step 3: Download Whisper ASR model
    let effective_model = if model.is_empty() {
        model_download::DEFAULT_WHISPER_MODEL
    } else {
        model
    };
    let expected_path = model_download::model_path(&data_dir, effective_model)?;
    println!("\n  [3/7] Downloading Whisper ASR model ({})...", effective_model);
    println!("  📁 Target: {}", expected_path.display());
    let _model_path = model_download::download_whisper_model(effective_model, &data_dir).await?;

    // Step 4: Download whisper-server binary
    println!("\n  [4/7] Downloading whisper-server binary...");
    let _ = model_download::download_whisper_binary(&data_dir).await;

    // Step 5: TTS — try Qwen first; if it fails, ensure Piper is fully set up.
    println!("\n  [5/7] Setting up TTS...");
    let qwen_ok = qwen_tts_process::setup_install(&data_dir).await;

    // Step 6: Piper TTS — always set up (primary when Qwen unavailable, fallback otherwise)
    println!("\n  [6/7] Setting up Piper TTS{}...",
        if qwen_ok { " (fallback)" } else { " (primary — Qwen unavailable)" });
    let piper_bin_ok = model_download::download_piper_binary(&data_dir).await.is_ok();
    let piper_model_ok = model_download::download_piper_model(&data_dir).await.is_ok();
    if !qwen_ok && (!piper_bin_ok || !piper_model_ok) {
        println!("  ⚠  Both Qwen TTS and Piper failed — voice output will be text-only.");
    }

    // Step 7: Download default LLM (Gemma 2 2B via llamafile)
    println!(
        "\n  [7/7] Downloading LLM model ({})...",
        model_download::DEFAULT_LLAMAFILE_MODEL
    );
    match model_download::download_llamafile_model(
        model_download::DEFAULT_LLAMAFILE_MODEL,
        &data_dir,
    )
    .await
    {
        Ok(p) => println!("  ✅ LLM model ready: {}", p.display()),
        Err(e) => println!("  ⚠  Could not download LLM model: {}", e),
    }

    println!();
    println!("  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  ✅ Setup complete!  Next steps:");
    println!();
    println!("  Run the server (all AI components start automatically):");
    println!("       pond-server serve");
    println!();
    println!("  Or run interactive CLI chat with voice + TTS:");
    println!("       pond-server chat --provider llamafile --input whisper --tts piper");
    println!("  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    Ok(())
}

async fn run_server(static_dir: std::path::PathBuf, open: bool, debug: bool, agent_backend: &str) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose In A Pond  v{}         ║", env!("CARGO_PKG_VERSION"));
    println!("  ╚═══════════════════════════════════════╝");

    // Initialize databases
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("goose-in-a-pond");
    let db = Database::init(&data_dir).await?;

    // Soft system-dep check (non-fatal — just warn if something looks wrong)
    system_deps::warn_if_missing();

    // ── Load registry + settings early (drives model selection) ─────────────
    let registry = model_registry::ModelRegistry::load_cached(&data_dir);
    let settings_repo_early = SqliteSettingsRepository::new(db.system.clone());
    let settings = settings_repo_early.get().await.unwrap_or_default();

    // ── Component startup: auto-download + wire critical services ────────────
    println!("\n  ── Components ──────────────────────────────────────");

    // STT — whisper.cpp binary + model (use active_whisper_model from settings)
    let active_whisper = registry
        .find_whisper(&settings.active_whisper_model)
        .unwrap_or_else(|| registry.whisper.first().expect("registry has no whisper models"));
    let whisper_model = data_dir.join("models").join(&active_whisper.filename);
    if !whisper_model.exists() {
        println!("  📥 STT model not found — downloading ({})...", active_whisper.name);
        match model_download::download_whisper_model(&active_whisper.name, &data_dir).await {
            Ok(_) => {}
            Err(e) => println!("  ⚠  STT model download failed: {}", e),
        }
    }
    if !model_download::whisper_binary_path(&data_dir).exists() {
        println!("  📥 STT binary not found — downloading...");
        match model_download::download_whisper_binary(&data_dir).await {
            Ok(_) => {}
            Err(e) => println!("  ⚠  STT binary download failed: {}", e),
        }
    }
    let (_whisper_guard, whisper_port) = whisper_process::try_start(&data_dir, &whisper_model).await;
    let whisper_url = whisper_process::url_for(whisper_port);

    // TTS — ensure Qwen TTS is installed, start it, then build Qwen (primary) → Piper (fallback)
    // try_start returns (process, confirmed_running). If it was already running before
    // we called try_start (returns None), fall back to a live is_running check.
    let (_qwen_tts_guard, qwen_tts_url, qwen_tts_available) =
        match qwen_tts_process::try_start(&data_dir).await {
            Some((proc, port, confirmed)) => {
                (Some(proc), qwen_tts_process::url_for(port), confirmed)
            }
            None => {
                let url = qwen_tts_process::url_for(ports::QWEN_TTS);
                let running = qwen_tts_process::is_running(&url).await;
                (None, url, running)
            }
        };

    let piper_model = model_download::tts_models_dir(&data_dir)
        .join(model_download::PIPER_MODEL_FILENAME);

    // Ensure piper binary + model + espeak-ng-data are present.
    if !model_download::piper_binary_path(&data_dir).exists() {
        let _ = model_download::download_piper_binary(&data_dir).await;
    }
    if !piper_model.exists() {
        let _ = model_download::download_piper_model(&data_dir).await;
    }
    // Always ensure espeak-ng-data is present (maybe missing after source build).
    model_download::ensure_espeak_ng_data(&data_dir).await;

    // Start piper as a persistent HTTP server so it shows up in the service list.
    let espeak_data = {
        let p = model_download::piper_espeak_data_path(&data_dir);
        if p.exists() { Some(p) } else { None }
    };
    let piper_tts: Option<Arc<dyn pond_core::ports::voice_output::VoiceOutput>> =
        match piper_process::find_binary(&data_dir) {
            Some(bin) if piper_model.exists() => {
                let ed = espeak_data.clone();
                match piper_http::start(bin.clone(), piper_model.clone(), ed).await {
                    Ok(port) => {
                        println!("  ✅ Piper TTS running on port {}", port);
                        let mut out = PiperOutput::new(bin, piper_model.clone());
                        if let Some(d) = espeak_data.clone() { out = out.with_espeak_data(d); }
                        Some(Arc::new(out))
                    }
                    Err(e) => {
                        tracing::warn!("piper-http failed to start: {e}");
                        let mut out = PiperOutput::new(bin, piper_model.clone());
                        if let Some(d) = espeak_data.clone() { out = out.with_espeak_data(d); }
                        Some(Arc::new(out))
                    }
                }
            }
            _ => None,
        };
    let tts: Option<Arc<dyn pond_core::ports::voice_output::VoiceOutput>> = if qwen_tts_available {
        let qwen_tts = Arc::new(
            QwenTtsOutput::new(Some(&qwen_tts_url))
                .with_voice(&settings.voice_tts_http_voice),
        );
        Some(match piper_tts {
            Some(piper) => {
                println!("  ✅ TTS: qwen-tts (primary) → piper (fallback)");
                Arc::new(FallbackVoiceOutput::new(qwen_tts, piper))
                    as Arc<dyn pond_core::ports::voice_output::VoiceOutput>
            }
            None => {
                println!("  ✅ TTS: qwen-tts (primary, no fallback available)");
                qwen_tts as Arc<dyn pond_core::ports::voice_output::VoiceOutput>
            }
        })
    } else {
        match piper_tts {
            Some(piper) => {
                println!("  ✅ TTS: piper (qwen-tts unavailable)");
                Some(piper)
            }
            None => {
                println!("  ⚠  TTS: no engine available — responses will be text-only");
                None
            }
        }
    };

    // LLM — llamafile auto-download + auto-start (use active_llm_model from settings)
    let active_llm = registry
        .find_llamafile(&settings.active_llm_model)
        .unwrap_or_else(|| registry.llamafile.first().expect("registry has no llamafile models"));
    let llm_model_path = llamafile_process::find_model(&data_dir);
    if llm_model_path.is_none() {
        println!("  📥 LLM model not found — downloading {}...", active_llm.name);
        match model_download::download_llamafile_model(&active_llm.name, &data_dir).await {
            Ok(_)  => {}
            Err(e) => println!("  ⚠  LLM download failed: {}", e),
        }
    }
    let (_llamafile_guard, llamafile_port) =
        match llamafile_process::try_start(&data_dir).await {
            Some((proc, port)) => (Some(proc), port),
            None => (None, ports::LLAMAFILE),
        };
    let llamafile_url = llamafile_process::url_for(llamafile_port);

    println!("  ────────────────────────────────────────────────────\n");

    // Build model status snapshot for API
    let model_status_entries = model_registry::build_model_status(
        &registry,
        &settings.active_whisper_model,
        &settings.active_llm_model,
        &settings.active_tts_model,
        &data_dir,
    );
    let model_status = Arc::new(tokio::sync::RwLock::new(model_status_entries));

    // Build app state
    let onboarding_repo = Arc::new(SqlxOnboardingRepository::new(db.system.clone()));
    let session_storage: Arc<dyn pond_core::ports::session_storage::SessionStorage> =
        Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let settings_repo: Arc<dyn pond_core::ports::settings::SettingsRepository + Send + Sync> =
        Arc::new(SqliteSettingsRepository::new(db.system.clone()));
    let profile_repo: Arc<dyn pond_core::ports::profile::ProfileRepository + Send + Sync> =
        Arc::new(SqliteProfileRepository::new(db.system.clone()));
    let device_registry: Arc<dyn pond_core::ports::device_registry::DeviceRegistry + Send + Sync> =
        Arc::new(SqliteDeviceRegistry::new(db.system.clone()));
    let memory_repo: Arc<dyn pond_core::ports::memory_repository::MemoryRepository + Send + Sync> =
        Arc::new(SqliteMemoryRepository::new(db.system.clone()));
    let sensor_storage: Arc<dyn pond_core::ports::sensor_storage::SensorStorage + Send + Sync> =
        Arc::new(SqliteSensorStorage::new(db.logs.clone()));
    let camera_storage: Arc<dyn pond_core::ports::camera_storage::CameraStorage + Send + Sync> =
        Arc::new(SqliteCameraStorage::new(db.logs.clone()));

    // ── One-time migration: bootstrap role fields from legacy single-model settings ──
    // If chat_model is empty (new fields not yet persisted), copy active_llm_model
    // and llm_provider so existing installs don't need to visit Settings.
    let (effective_chat_provider, effective_chat_model) =
        if settings.chat_model.is_empty() {
            let _ = settings_repo.set_key("chat_provider", settings.llm_provider.clone()).await;
            let _ = settings_repo.set_key("chat_model",    settings.active_llm_model.clone()).await;
            (settings.llm_provider.clone(), settings.active_llm_model.clone())
        } else {
            (settings.chat_provider.clone(), settings.chat_model.clone())
        };

    // ── Build per-role LLM providers ────────────────────────────────────────
    // Each role (Chat / Think / Task) may use a different provider + model.
    // Token budget and temperature are baked in at startup.
    //
    // Helper: build one Arc<dyn LlmProvider> for a given (provider, model) pair.
    let build_provider = |provider: &str, model: &str| -> Arc<dyn LlmProvider> {
        match provider {
            "ollama" => Arc::new(
                OllamaProvider::new(None, Some(model))
                    .with_max_tokens(settings.llm_max_tokens)
                    .with_temperature(settings.llm_temperature),
            ) as Arc<dyn LlmProvider>,
            _ => Arc::new(
                // Default: llamafile (covers "llamafile" and unknown values)
                LlamafileProvider::new(Some(&llamafile_url))
                    .with_max_tokens(settings.llm_max_tokens)
                    .with_temperature(settings.llm_temperature),
            ) as Arc<dyn LlmProvider>,
        }
    };

    let chat_provider_arc = build_provider(&effective_chat_provider, &effective_chat_model);

    // Think role: reuse chat Arc if not separately configured.
    let think_provider_arc: Arc<dyn LlmProvider> =
        if let (Some(tp), Some(tm)) = (&settings.think_provider, &settings.think_model) {
            build_provider(tp, tm)
        } else {
            chat_provider_arc.clone()
        };

    // Task role: reuse chat Arc if not separately configured.
    let task_provider_arc: Arc<dyn LlmProvider> =
        if let (Some(tp), Some(tm)) = (&settings.task_provider, &settings.task_model) {
            build_provider(tp, tm)
        } else {
            chat_provider_arc.clone()
        };

    let llm_provider: Option<Arc<dyn LlmProvider>> = Some(Arc::new(
        ModelRouter::new(chat_provider_arc, think_provider_arc, task_provider_arc)
    ));

    let db = Arc::new(db);

    // Spawn background TTL pruning task (runs every 6 hours)
    {
        let logs = db.logs.clone();
        let system = db.system.clone();
        tokio::spawn(async move {
            pond_infra::pruning::run_pruning(logs, system, Default::default()).await;
        });
    }

    // Debug mode: tail pond_logs.db so new event_log rows are printed to the
    // terminal in real time. Polls every second and only surfaces rows added
    // after startup, so existing history is not replayed.
    if debug {
        let logs_pool = db.logs.clone();
        tokio::spawn(async move {
            tail_event_log(logs_pool).await;
        });
    }

    // Weather — fed into GiapServiceHandles (MCP tool), not AppState.
    // The LLM calls giap__get_current_weather when it needs weather data.
    let weather: Option<Arc<dyn WeatherProvider>> = {
        if settings.weather_enabled
            && (settings.weather_latitude != 0.0 || settings.weather_longitude != 0.0)
        {
            let loc = if settings.weather_location_name.is_empty() {
                format!("{:.3}, {:.3}", settings.weather_latitude, settings.weather_longitude)
            } else {
                settings.weather_location_name.clone()
            };
            tracing::info!(
                "weather enabled: {} ({}, {})",
                loc, settings.weather_latitude, settings.weather_longitude
            );
            Some(Arc::new(OpenMeteoWeatherAdapter::new(
                settings.weather_latitude,
                settings.weather_longitude,
                loc,
            )))
        } else {
            tracing::info!("weather disabled — enable via PUT /api/v1/settings (weather_enabled + lat/lon)");
            None
        }
    };

    // Scheduler — persist task list next to the databases
    let scheduler: Option<Arc<dyn pond_core::ports::scheduler::SchedulerPort>> = {
        let exec = Arc::new(WebhookTaskExecutor::new());
        match CronSchedulerAdapter::new(data_dir.join("schedules.json"), exec).await {
            Ok(s) => {
                tracing::info!("scheduler ready ({})", data_dir.join("schedules.json").display());
                Some(Arc::new(s))
            }
            Err(e) => {
                tracing::warn!("scheduler init failed: {e} — schedule endpoints will return 503");
                None
            }
        }
    };

    // MCP Memory — enabled when --features mcp-memory is passed at build time.
    #[cfg(feature = "mcp-memory")]
    let mcp_memory: Option<Arc<dyn pond_core::ports::mcp_memory::McpMemoryPort + Send + Sync>> = {
        use pond_adapters_mcp_memory::GooseMcpMemoryAdapter;
        let adapter = GooseMcpMemoryAdapter::new(data_dir.join("memory"));
        tracing::info!("MCP memory enabled ({})", data_dir.join("memory").display());
        Some(Arc::new(adapter))
    };
    #[cfg(not(feature = "mcp-memory"))]
    let mcp_memory: Option<Arc<dyn pond_core::ports::mcp_memory::McpMemoryPort + Send + Sync>> = None;

    // ── Agent backend ────────────────────────────────────────────────────────────
    #[cfg(feature = "goose-agent")]
    let (agent, extension_manager) = build_goose_backend(
        agent_backend,
        &llamafile_url,
        weather.clone(),
        device_registry.clone(),
        scheduler.clone(),
    ).await;

    #[cfg(not(feature = "goose-agent"))]
    let (agent, extension_manager): (Arc<dyn Agent>, Option<Arc<dyn pond_core::ports::extension_manager::ExtensionManagerPort>>) = {
        if agent_backend == "goose" {
            tracing::warn!(
                "Goose agent backend requested but this binary was compiled without the `goose-agent` feature. \
                 Rebuild with: cargo run -p pond-server -- serve  (goose-agent is a default feature). \
                 Falling back to mock agent."
            );
        }
        (Arc::new(MockAgent::new()), None)
    };

    // MCP client — load persisted server configs and auto-connect enabled ones.
    let mcp_server_repo: Option<Arc<dyn pond_core::ports::mcp_server::McpServerRepository>> = {
        let repo = Arc::new(SqliteMcpServerRepository::new(db.system.clone()));
        // Auto-connect saved external MCP servers if the extension manager is available.
        if let Some(mgr) = &extension_manager {
            match repo.list().await {
                Ok(servers) => {
                    for srv in servers.into_iter().filter(|s: &pond_core::ports::mcp_server::McpServerConfig| s.enabled) {
                        use pond_core::ports::extension_manager::AddExtensionRequest;
                        let req = AddExtensionRequest {
                            name:        srv.name.clone(),
                            kind:        srv.kind.clone(),
                            description: srv.description.clone(),
                            command:     srv.command.clone(),
                            args:        srv.args.clone(),
                            env:         srv.env.clone(),
                            uri:         srv.uri.clone(),
                        };
                        match mgr.add_extension(req).await {
                            Ok(_) => tracing::info!("auto-connected MCP server '{}'", srv.name),
                            Err(e) => tracing::warn!("failed to auto-connect MCP server '{}': {e}", srv.name),
                        }
                    }
                }
                Err(e) => tracing::warn!("failed to load saved MCP servers: {e}"),
            }
        }
        Some(repo)
    };

    let state = Arc::new(AppState {
        db,
        onboarding_repo,
        handshake: Arc::new(MockHandshake::new()),
        whisper_url: whisper_url.clone(),
        session_storage,
        http_client: reqwest::Client::new(),
        agent,
        llm_provider,
        tts,
        settings_repo,
        profile_repo,
        device_registry,
        memory_repo,
        embedding_provider: None,
        sensor_storage,
        camera_storage,
        prompt_template_dir: Some(data_dir.join("prompts")),
        model_status: Some(model_status),
        data_dir: Some(data_dir.clone()),
        skip_onboarding: false,
        scheduler,
        model_scheduler: None, // populated when local-inference feature is active
        mcp_memory,
        extension_manager,
        mcp_server_repo,
        qwen_tts_url: if qwen_tts_available { Some(qwen_tts_url.clone()) } else { None },
    });

    // Warn if static assets haven't been built yet
    if !static_dir.exists() {
        tracing::warn!(
            "Static dir {:?} not found — web dashboard will not be served. \
             Run `cd web && npm run build` to build it.",
            static_dir
        );
    }

    // Build router
    let app = pond_api::build_router(state, static_dir);

    // Resolve hostname — strip trailing ".local" if the OS already appended it
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "localhost".to_string());
    let hostname = hostname.strip_suffix(".local").unwrap_or(&hostname).to_string();

    let (listener, api_port) = ports::bind_with_fallback("0.0.0.0", ports::API_SERVER).await?;
    let display_url = if api_port == 80 {
        format!("http://pond.{}.local", hostname)
    } else {
        format!("http://pond.{}.local:{}", hostname, api_port)
    };

    println!("  🌐 Listening on 0.0.0.0:{}", api_port);
    println!("  📡 Dashboard: {}", display_url);
    println!("  📡 API:       {}/api/v1/health", display_url);
    println!();

    // Open the browser when:
    //   - `--open` is explicitly passed, OR
    //   - `--debug` is passed and the host has a graphical display.
    // On Linux a display requires DISPLAY (X11) or WAYLAND_DISPLAY to be set.
    // On macOS and Windows a display is always assumed to be present.
    if open || (debug && has_display()) {
        let url = format!("http://localhost:{}", api_port);
        if webbrowser::open(&url).is_err() {
            tracing::warn!("Could not open browser — no display available or xdg-open missing");
        }
    }

    axum::serve(listener, app).await?;

    Ok(())
}

async fn run_chat(provider: Option<&str>, model: Option<&str>, input: &str, wake_word: Option<&str>, no_wake_word: bool, tts: &str, tts_model: Option<std::path::PathBuf>) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose-in-a-Pond  v{}       ║", env!("CARGO_PKG_VERSION"));
    println!("  ║   Wait → Listen → Think → Speak      ║");
    println!("  ╚═══════════════════════════════════════╝");

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("goose-in-a-pond");
    let db = Database::init(&data_dir).await?;

    // Load settings early — drives provider, model, token budget, temperature, and wake word.
    // Falls back to Settings::default() when the DB has no rows yet (first run).
    let settings_repo_chat = SqliteSettingsRepository::new(db.system.clone());
    let settings = settings_repo_chat.get().await.unwrap_or_default();

    // CLI args override settings; settings override built-in defaults.
    let effective_provider = provider.unwrap_or(settings.llm_provider.as_str());
    let effective_model    = model.unwrap_or(settings.active_llm_model.as_str());
    println!("  Provider: {} (model: {})", effective_provider, effective_model);

    // Auto-start whisper.cpp when voice input is requested.
    let mut whisper_port = ports::WHISPER;
    let _whisper_guard = if input == "whisper" {
        let whisper_model = model_download::model_path(&data_dir, model_download::DEFAULT_WHISPER_MODEL)
            .unwrap_or_else(|_| data_dir.join("models").join("ggml-base.en.bin"));
        if !whisper_model.exists() {
            println!("  📥 STT model not found — downloading ({})...", model_download::DEFAULT_WHISPER_MODEL);
            match model_download::download_whisper_model(model_download::DEFAULT_WHISPER_MODEL, &data_dir).await {
                Ok(_)  => {}
                Err(e) => println!("  ⚠  STT model download failed: {}", e),
            }
        }
        let (guard, port) = whisper_process::try_start(&data_dir, &whisper_model).await;
        whisper_port = port;
        guard
    } else {
        None
    };
    let whisper_url = whisper_process::url_for(whisper_port);

    // Auto-start llamafile for all providers except ollama and local (which manage their own process).
    // llamafile is the default and fallback — always start it unless a remote/in-process provider is used.
    let mut llamafile_port = ports::LLAMAFILE;
    let _llamafile_guard = if effective_provider != "ollama" && effective_provider != "local" {
        if llamafile_process::find_model(&data_dir).is_none() {
            println!("  📥 LLM model not found — downloading {}...",
                model_download::DEFAULT_LLAMAFILE_MODEL);
            match model_download::download_llamafile_model(
                model_download::DEFAULT_LLAMAFILE_MODEL, &data_dir,
            ).await {
                Ok(_)  => {}
                Err(e) => println!("  ⚠  LLM download failed: {}", e),
            }
        }
        match llamafile_process::try_start(&data_dir).await {
            Some((proc, port)) => { llamafile_port = port; Some(proc) }
            None => None,
        }
    } else {
        None
    };
    let llamafile_url = llamafile_process::url_for(llamafile_port);

    let session_id = "default-session".to_string();
    let agent = Arc::new(MockAgent::new());

    // Resolve the system prompt using the already-loaded settings:
    // 1. File at $DATA_DIR/prompts/system.md (deployment override, rendered with all vars)
    // 2. build_system_prompt(&settings) — honours custom_system_prompt + prompt_style + addendum
    let system_prompt = {
        let prompt_dir    = data_dir.join("prompts");
        let file_template = std::fs::read_to_string(prompt_dir.join("system.md")).ok();
        match file_template {
            Some(tmpl) => {
                println!("  Prompt:   custom ({})", prompt_dir.join("system.md").display());
                let name     = pond_core::prompts::sanitize_field(&settings.assistant_name, 50);
                let user     = pond_core::prompts::sanitize_field(&settings.user_name, 50);
                let persona  = pond_core::prompts::sanitize_field(&settings.assistant_personality, 200);
                let tz       = pond_core::prompts::sanitize_field(&settings.timezone, 50);
                let location = if settings.weather_location_name.is_empty() {
                    String::new()
                } else {
                    format!("\nLocation: {}.", pond_core::prompts::sanitize_field(&settings.weather_location_name, 100))
                };
                let addendum = pond_core::prompts::sanitize_field(&settings.prompt_addendum, 500);
                pond_core::prompts::render_template(&tmpl, &[
                    ("assistant_name",  name.as_str()),
                    ("user_name",       user.as_str()),
                    ("personality",     persona.as_str()),
                    ("timezone",        tz.as_str()),
                    ("location",        location.as_str()),
                    ("prompt_addendum", addendum.as_str()),
                ])
            }
            None => {
                println!(
                    "  Assistant: {} / style: {} / user: {}",
                    settings.assistant_name, settings.prompt_style, settings.user_name
                );
                build_system_prompt(&settings)
            }
        }
    };

    let storage: Arc<dyn SessionStorage> = Arc::new(SqliteSessionStorage::new(db.system));
    // Create session if it doesn't exist; ignore duplicate-key errors from prior runs
    if let Err(e) = storage.create_session(session_id.clone()).await {
        match e {
            pond_core::ports::session_storage::SessionStorageError::StorageError(_) => {
                // Likely a duplicate key — session already exists, which is fine
                tracing::debug!("Session already exists, reusing: {}", session_id);
            }
            other => return Err(other.into()),
        }
    }

    let mut chat_service = ChatService::new(agent, session_id.clone(), storage)
        .with_system_prompt(system_prompt);

    // ── Wire LLM provider (settings drive token budget + temperature) ──
    // Default / fallback is always llamafile — it is auto-started above for any provider
    // that is not "ollama" or "local".
    match effective_provider {
        "ollama" => {
            println!(
                "  Model:    {} (ollama @ {}, max_tokens={}, temp={})",
                effective_model,
                pond_adapters_ollama::DEFAULT_HOST,
                settings.llm_max_tokens,
                settings.llm_temperature,
            );
            let llm = Arc::new(
                OllamaProvider::new(None, Some(effective_model))
                    .with_max_tokens(settings.llm_max_tokens)
                    .with_temperature(settings.llm_temperature),
            );
            chat_service = chat_service.with_provider(llm);
        }
        "local" => {
            #[cfg(feature = "local-inference")]
            {
                use pond_adapters_local_inference::LocalInferenceLlmAdapter;
                let model_id = effective_model;
                println!("  Model:    {} (local GGUF in-process)", model_id);
                let llm = Arc::new(LocalInferenceLlmAdapter::new(model_id).await?);
                chat_service = chat_service.with_provider(llm);
            }
            #[cfg(not(feature = "local-inference"))]
            {
                eprintln!(
                    "  ERROR: --provider local requires the `local-inference` feature.\n\
                     Rebuild with:\n  \
                     cargo run -p pond-server --features local-inference -- chat --provider local"
                );
                std::process::exit(1);
            }
        }
        _ => {
            // "llamafile" and any unrecognised value — use the llamafile process started above.
            println!(
                "  Model:    {} (llamafile @ {}, max_tokens={}, temp={})",
                pond_adapters_llamafile::DEFAULT_MODEL,
                llamafile_url,
                settings.llm_max_tokens,
                settings.llm_temperature,
            );
            let llm = Arc::new(
                LlamafileProvider::new(Some(&llamafile_url))
                    .with_max_tokens(settings.llm_max_tokens)
                    .with_temperature(settings.llm_temperature),
            );
            chat_service = chat_service.with_provider(llm);
        }
    }

    // ── Wire voice input ──
    let voice: Arc<dyn VoiceInput> = match input {
        "whisper" => {
            println!("  Input:    whisper (@ {})", whisper_url);
            Arc::new(WhisperInput::new(Some(&whisper_url)))
        }
        _ => {
            println!("  Input:    stdin");
            Arc::new(StdinInput::new())
        }
    };
    chat_service = chat_service.with_voice_input(voice);

    // ── Wire wake word detector ──
    let detector: Arc<dyn WakeWordDetector> = if no_wake_word || input != "whisper" {
        Arc::new(InstantActivation)
    } else {
        let trigger = wake_word.unwrap_or(settings.voice_wake_word.as_str());
        println!("  Wake word: \"{}\" (via whisper @ {})", trigger, whisper_url);
        Arc::new(WhisperKeywordDetector::new(Some(&whisper_url), trigger))
    };
    chat_service = chat_service.with_wake_word_detector(detector);

    // Auto-start Qwen TTS server before the match (guard must outlive voice_out).
    let mut qwen_chat_port = ports::QWEN_TTS;
    let _qwen_tts_chat_guard = if tts == "qwen" || tts == "qwen-tts" {
        match qwen_tts_process::try_start(&data_dir).await {
            Some((proc, port, _confirmed)) => { qwen_chat_port = port; Some(proc) }
            None => None,
        }
    } else {
        None
    };
    let qwen_chat_url = qwen_tts_process::url_for(qwen_chat_port);

    // ── Wire TTS output ──
    let voice_out: Arc<dyn VoiceOutput> = match tts {
        "qwen" | "qwen-tts" => {
            let qwen = Arc::new(QwenTtsOutput::new(Some(&qwen_chat_url)));

            // Build piper if available (auto-download if needed).
            let piper_model = data_dir.join("models").join("tts").join(model_download::PIPER_MODEL_FILENAME);
            if piper_process::find_binary(&data_dir).is_none() {
                let _ = model_download::download_piper_binary(&data_dir).await;
            }
            if !piper_model.exists() {
                let _ = model_download::download_piper_model(&data_dir).await;
            }
            match piper_process::find_binary(&data_dir) {
                Some(bin) if piper_model.exists() => {
                    println!("  TTS:      qwen-tts (primary) → piper (fallback)");
                    Arc::new(FallbackVoiceOutput::new(
                        qwen as Arc<dyn VoiceOutput>,
                        Arc::new(PiperOutput::new(bin, piper_model)),
                    ))
                }
                _ => {
                    println!("  TTS:      qwen-tts");
                    qwen as Arc<dyn VoiceOutput>
                }
            }
        }
        "piper" => {
            let model_path = tts_model.unwrap_or_else(|| {
                data_dir.join("models").join("tts").join(model_download::PIPER_MODEL_FILENAME)
            });
            if piper_process::find_binary(&data_dir).is_none() {
                println!("  📥 TTS binary not found — downloading...");
                match model_download::download_piper_binary(&data_dir).await {
                    Ok(_)  => {}
                    Err(e) => println!("  ⚠  TTS binary download failed: {}", e),
                }
            }
            if !model_path.exists() {
                println!("  📥 TTS model not found — downloading...");
                match model_download::download_piper_model(&data_dir).await {
                    Ok(_)  => {}
                    Err(e) => println!("  ⚠  TTS model download failed: {}", e),
                }
            }
            match piper_process::find_binary(&data_dir) {
                Some(bin) => {
                    println!("  TTS:      piper ({})", model_path.file_name().unwrap_or_default().to_string_lossy());
                    Arc::new(PiperOutput::new(bin, model_path))
                }
                None => {
                    println!("  TTS:      piper unavailable (binary not found) — falling back to print");
                    Arc::new(PrintOutput)
                }
            }
        }
        _ => {
            println!("  TTS:      print");
            Arc::new(PrintOutput)
        }
    };
    chat_service = chat_service.with_voice_output(voice_out);

    chat_service.run_loop().await?;

    Ok(())
}

/// Returns `true` when the process has access to a graphical display.
///
/// On Linux, a display is present when `DISPLAY` (X11) or `WAYLAND_DISPLAY`
/// is set in the environment. On all other platforms (macOS, Windows) a
/// display is unconditionally assumed.
fn has_display() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Background task that tails the `event_log` table in `pond_logs.db`.
///
/// On startup, it records the current maximum row ID so that pre-existing log
/// history is not replayed. It then polls every second and prints any new rows
/// to stdout. This is intentionally a plain `println!` rather than a tracing
/// event so the output is always visible alongside the tracing output, making
/// it easy to correlate API activity with DB-level events in a single terminal.
///
/// Output format:
/// ```text
///   [db] 2024-01-15 12:34:56  INFO [pond-api] request handled
///   [db] 2024-01-15 12:34:57 ERROR [pond-core] something failed — {"key":"val"}
/// ```
async fn tail_event_log(pool: sqlx::Pool<sqlx::Sqlite>) {
    // Anchor to the highest existing ID so we only surface new events.
    let mut cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM event_log")
        .fetch_one(&pool)
        .await
        .unwrap_or(0);

    println!("  [debug] tailing pond_logs.db event_log (cursor = {})...", cursor);

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let rows: Vec<(i64, String, String, String, String, Option<String>)> =
            sqlx::query_as(
                "SELECT id, timestamp, level, source, message, metadata \
                 FROM event_log WHERE id > ? ORDER BY id ASC",
            )
            .bind(cursor)
            .fetch_all(&pool)
            .await
            .unwrap_or_default();

        for (id, timestamp, level, source, message, metadata) in rows {
            match metadata.as_deref().filter(|m| !m.is_empty()) {
                Some(meta) => println!(
                    "  [db] {} {:>5} [{}] {} — {}",
                    timestamp, level, source, message, meta
                ),
                None => println!(
                    "  [db] {} {:>5} [{}] {}",
                    timestamp, level, source, message
                ),
            }
            cursor = id;
        }
    }
}

async fn run_status() -> Result<()> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let hostname = hostname.strip_suffix(".local").unwrap_or(&hostname).to_string();

    println!("  🦆 Goose In A Pond — Status");
    println!("  ─────────────────────────────");
    println!("  Version:   {}", env!("CARGO_PKG_VERSION"));
    println!("  Hostname:  {}", hostname);
    println!("  Platform:  {} / {}", std::env::consts::OS, std::env::consts::ARCH);

    // TODO: Check DB status, onboarding state, running services
    println!("  Database:  TODO — check connection");
    println!("  Onboarded: TODO — check onboarding state");

    Ok(())
}

fn get_local_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.168.1.1:80").ok()?;
    let local_addr = socket.local_addr().ok()?;
    Some(local_addr.ip().to_string())
}

fn prompt_nonempty(prompt: &str) -> Result<String> {

    loop {
        print!("{}", prompt);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        let trimmed = input.trim();

        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }

        println!("Input cannot be empty. Try again.");
    }
}
async fn run_main_menu() -> Result<()> {
    loop {
        println!("\n🦆 Goose In A Pond — Main Menu");
        println!("─────────────────────────────");
        println!("  1) Chat        — Interactive AI chat");
        println!("  2) Serve       — Start the HTTP server + API");
        println!("  3) Status      — Show system info");
        println!("  4) Exit");
        println!();

        let choice = prompt_nonempty("Choose an option: ")?;

        match choice.trim() {
            "1" => {
                run_chat(None, None, "stdin", None, true, "none", None).await?;
            }
            "2" => {
                run_server(std::path::PathBuf::from("web/dist"), false, false, "goose").await?;
            }
            "3" => {
                run_status().await?;
            }
            "4" => {
                println!("Goodbye!");
                break;
            }
            _ => {
                println!("Invalid option. Try again.");
            }
        }
    }

    Ok(())
}
async fn run_onboard(reset: bool) -> Result<()> {
    println!("🦆 Goose In A Pond — Interactive Onboarding Wizard\n");

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("goose-in-a-pond");
    let db = Database::init(&data_dir).await?;
    let repo = SqlxOnboardingRepository::new(db.system.clone());
    let service = OnboardingService::new(repo);

    if reset {
        service.reset().await?;
        println!("Onboarding reset. Starting from scratch...\n");
    }

    // TODO: persist user_data once UserProfile domain + port exist
    let mut user_data: HashMap<String, String> = HashMap::new();

    loop {
        let current_step = service.status().await;

        match current_step {
            None => {
                println!("Starting onboarding from scratch...");
                service.start().await?;
            }

            Some(OnboardingStep::Welcome) => {
                println!("Step: Welcome");

                if let Some(ip) = get_local_ip() {
                    println!("Detected device IP: {}", ip);
                    user_data.insert("device_ip".to_string(), ip);
                } else {
                    println!("Could not detect IP, using default 'unknown'.");
                    user_data.insert("device_ip".to_string(), "unknown".to_string());
                }

                service.advance().await?;
            }

            Some(OnboardingStep::Basics) => {
                println!("Step: Basics");

                let username = prompt_nonempty("Enter your name: ")?;
                user_data.insert("user_name".to_string(), username);

                service.advance().await?;
            }

            Some(OnboardingStep::Location) => {
                println!("Step: Language & Location");
                println!("(Press Enter to skip any field — configure later in Settings)");

                print!("Timezone (e.g. Africa/Nairobi): ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut timezone = String::new();
                let _ = std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut timezone);
                if !timezone.trim().is_empty() {
                    user_data.insert("timezone".to_string(), timezone.trim().to_string());
                }

                service.advance().await?;
            }

            Some(OnboardingStep::Accessibility) => {
                println!("Step: Accessibility");
                println!("(All accessibility options can be configured in Settings later)");
                service.advance().await?;
            }

            Some(OnboardingStep::Personality) => {
                println!("Step: Personality");

                let styles = vec!["balanced", "concise", "technical", "warm"];
                println!("Choose a conversation style:");
                for (i, s) in styles.iter().enumerate() {
                    println!("  {}) {}", i + 1, s);
                }

                let selected = loop {
                    let choice = prompt_nonempty("Enter the number of your choice: ")?;
                    if let Ok(index) = choice.parse::<usize>() {
                        if index >= 1 && index <= styles.len() {
                            break styles[index - 1].to_string();
                        }
                    }
                    println!("Invalid choice. Try again.");
                };

                println!("You selected: {}", selected);
                user_data.insert("prompt_style".to_string(), selected);

                service.advance().await?;
            }

            Some(OnboardingStep::GooseIdentity) => {
                println!("Step: Goose's Identity");

                print!("Assistant name (default: Goose): ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut name = String::new();
                let _ = std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut name);
                let name = if name.trim().is_empty() { "Goose".to_string() } else { name.trim().to_string() };
                user_data.insert("assistant_name".to_string(), name);

                service.advance().await?;
            }

            Some(OnboardingStep::WakeWord) => {
                println!("Step: Wake Word");
                println!("Presets: 1) goose  2) hey goose  3) ok computer  4) custom");

                let wake_word = loop {
                    let choice = prompt_nonempty("Enter number or type a custom phrase: ")?;
                    break match choice.trim() {
                        "1" => "goose".to_string(),
                        "2" => "hey goose".to_string(),
                        "3" => "ok computer".to_string(),
                        "4" => prompt_nonempty("Enter your custom wake phrase: ")?,
                        other => other.to_string(),
                    };
                };

                user_data.insert("voice_wake_word".to_string(), wake_word);
                service.advance().await?;
            }

            Some(OnboardingStep::Model) => {
                println!("Step: AI Model");
                let model = prompt_nonempty("Enter the model name (e.g. gemma-2b): ")?;
                user_data.insert("active_llm_model".to_string(), model);
                service.advance().await?;
            }

            Some(OnboardingStep::Extensions) => {
                println!("Step: Extensions");
                println!("(Extensions can be enabled from Settings later)");
                service.advance().await?;
            }

            Some(OnboardingStep::Completed) => {
                if user_data.is_empty() {
                    println!("You are already onboarded!");
                    println!("Run with --reset to start over.");
                } else {
                    println!("\n Onboarding complete! Here’s your info:\n");
                    for (key, value) in &user_data {
                        println!("  {}: {}", key, value);
                    }
                }
                run_main_menu().await?;
                break;
            }
        }
    }

    Ok(())
}

// ── Goose agent backend ───────────────────────────────────────────────────────

/// Build a Goose-backed agent + extension manager.
///
/// Only compiled when the `goose-agent` feature is enabled (default).
/// Falls back to MockAgent when `--agent mock` is explicitly passed.
#[cfg(feature = "goose-agent")]
async fn build_goose_backend(
    agent_backend: &str,
    llamafile_url: &str,
    weather: Option<Arc<dyn WeatherProvider>>,
    device_registry: Arc<dyn pond_core::ports::device_registry::DeviceRegistry + Send + Sync>,
    scheduler: Option<Arc<dyn pond_core::ports::scheduler::SchedulerPort>>,
) -> (
    Arc<dyn Agent>,
    Option<Arc<dyn pond_core::ports::extension_manager::ExtensionManagerPort>>,
) {
    use pond_adapters_goose::{GiapServiceHandles, GooseAdapter, register_giap_extension};
    use pond_core::ports::extension_manager::ExtensionManagerPort;

    if agent_backend != "goose" {
        return (Arc::new(MockAgent::new()), None);
    }

    // Register the GIAP MCP server into Goose's builtin extension registry.
    let handles = Arc::new(GiapServiceHandles { weather, device_registry, scheduler });
    if let Err(e) = register_giap_extension(handles) {
        tracing::error!("GIAP MCP registration failed: {e} — falling back to mock agent");
        return (Arc::new(MockAgent::new()), None);
    }

    // Build the adapter (OllamaProvider → llamafile on actual port).
    match GooseAdapter::with_llamafile(Some(llamafile_url)).await {
        Ok(adapter) => {
            let ext_mgr: Arc<dyn ExtensionManagerPort> =
                Arc::new(adapter.extension_manager("server".to_string()));
            tracing::info!("Goose agent active — GIAP MCP extension registered");
            let agent: Arc<dyn Agent> = Arc::new(adapter);
            (agent, Some(ext_mgr))
        }
        Err(e) => {
            tracing::error!("GooseAdapter init failed: {e} — falling back to mock agent");
            (Arc::new(MockAgent::new()), None)
        }
    }
}
