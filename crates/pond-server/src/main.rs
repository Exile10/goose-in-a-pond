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
mod piper_process;
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
use pond_core::ports::settings::SettingsRepository as _;
use pond_core::prompts::build_system_prompt;
use pond_core::services::chat::ChatService;
use pond_core::services::fallback_provider::FallbackProvider;
use pond_core::services::fallback_voice_output::FallbackVoiceOutput;
use pond_core::services::mock_agent::MockAgent;
use pond_core::services::stdin_input::StdinInput;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_device_registry::SqliteDeviceRegistry;
use pond_infra::sqlite_memory::SqliteMemoryRepository;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_sensor::{SqliteCameraStorage, SqliteSensorStorage};
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
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
        /// Port to listen on (default: 4000)
        #[arg(short, long, default_value = "4000")]
        port: u16,

        /// Path to the built web dashboard assets (run `cd web && npm run build` first)
        #[arg(long, default_value = "web/dist")]
        static_dir: std::path::PathBuf,

        /// Open the dashboard in the browser
        #[arg(long)]
        open: bool,

        /// Enable debug logging
        #[arg(long)]
        debug: bool,
    },

    /// Interactive CLI chat (Wait→Listen→Think→Speak loop)
    Chat {
        /// LLM provider: mock, llamafile, or ollama
        #[arg(short = 'P', long, default_value = "mock")]
        provider: String,

        /// Model name (only used when --provider ollama, e.g. "llama3.2", "gemma2")
        #[arg(short = 'M', long)]
        model: Option<String>,

        /// Input source: stdin (text) or whisper (microphone → ASR)
        #[arg(short = 'I', long, default_value = "stdin")]
        input: String,

        /// URL of the running whisper.cpp server (only used when --input whisper)
        #[arg(long)]
        whisper_url: Option<String>,

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
        Some(Commands::Serve { port, static_dir, open, debug }) => {
            init_tracing(debug);
            run_server(port, static_dir, open).await
        }
        Some(Commands::Chat { provider, model, input, whisper_url, wake_word, no_wake_word, tts, tts_model }) => {
            init_tracing(false);
            run_chat(&provider, model.as_deref(), &input, whisper_url.as_deref(), wake_word.as_deref(), no_wake_word, &tts, tts_model).await
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
            // Default: run interactive chat (backward compat)
            init_tracing(false);
            run_chat("mock", None, "stdin", None, None, true, "none", None).await
        }
    }
}

fn init_tracing(debug: bool) {
    let level = if debug { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| level.into()),
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

async fn run_server(port: u16, static_dir: std::path::PathBuf, open: bool) -> Result<()> {
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
    let _whisper_guard = whisper_process::try_start(&data_dir, &whisper_model, 9000).await;

    // TTS — ensure Qwen TTS is installed, start it, then build Qwen (primary) → Piper (fallback)
    let _qwen_tts_guard = qwen_tts_process::try_start(&settings.voice_tts_http_url, &data_dir).await;
    let qwen_tts_available = qwen_tts_process::is_running(&settings.voice_tts_http_url).await;

    let piper_model = model_download::tts_models_dir(&data_dir)
        .join(model_download::PIPER_MODEL_FILENAME);

    // If qwen is not available, piper is the primary TTS — ensure it is fully set up.
    if !model_download::piper_binary_path(&data_dir).exists() {
        let _ = model_download::download_piper_binary(&data_dir).await;
    }
    if !piper_model.exists() {
        let _ = model_download::download_piper_model(&data_dir).await;
    }

    let piper_tts: Option<Arc<dyn pond_core::ports::voice_output::VoiceOutput>> =
        match piper_process::find_binary(&data_dir) {
            Some(bin) if piper_model.exists() => {
                Some(Arc::new(PiperOutput::new(bin, piper_model.clone())))
            }
            _ => None,
        };
    let tts: Option<Arc<dyn pond_core::ports::voice_output::VoiceOutput>> = if qwen_tts_available {
        let qwen_tts = Arc::new(
            QwenTtsOutput::new(Some(&settings.voice_tts_http_url))
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
    let _llamafile_guard = llamafile_process::try_start(&data_dir, 8080).await;

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

    let agent: Arc<dyn Agent> = Arc::new(MockAgent::new());
    // Fallback chain: llamafile → ollama → (if both down, ChatService falls back to agent echo)
    let llamafile = Arc::new(LlamafileProvider::new(None).with_max_tokens(1024));
    let ollama = Arc::new(OllamaProvider::new(None, None).with_max_tokens(1024));
    let llm_provider: Option<Arc<dyn LlmProvider>> = Some(
        Arc::new(FallbackProvider::new(llamafile, ollama))
    );

    let db = Arc::new(db);

    // Spawn background TTL pruning task (runs every 6 hours)
    {
        let logs = db.logs.clone();
        let system = db.system.clone();
        tokio::spawn(async move {
            pond_infra::pruning::run_pruning(logs, system, Default::default()).await;
        });
    }

    let state = Arc::new(AppState {
        db,
        onboarding_repo,
        handshake: Arc::new(MockHandshake::new()),
        whisper_url: "http://127.0.0.1:9000".to_string(),
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

    let bind_addr = format!("0.0.0.0:{}", port);
    let display_url = if port == 80 {
        format!("http://pond.{}.local", hostname)
    } else {
        format!("http://pond.{}.local:{}", hostname, port)
    };

    println!("  🌐 Listening on {}", bind_addr);
    println!("  📡 Dashboard: {}", display_url);
    println!("  📡 API:       {}/api/v1/health", display_url);
    println!();

    if open {
        let url = format!("http://localhost:{}", port);
        if webbrowser::open(&url).is_err() {
            tracing::warn!("Could not open browser (headless mode?)");
        }
    }

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn run_chat(provider: &str, model: Option<&str>, input: &str, whisper_url: Option<&str>, wake_word: Option<&str>, no_wake_word: bool, tts: &str, tts_model: Option<std::path::PathBuf>) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose-in-a-Pond  v{}       ║", env!("CARGO_PKG_VERSION"));
    println!("  ║   Wait → Listen → Think → Speak      ║");
    println!("  ╚═══════════════════════════════════════╝");
    println!("  Provider: {}", provider);

    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("goose-in-a-pond");
    let db = Database::init(&data_dir).await?;

    // Auto-start whisper.cpp when voice input is requested.
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
        whisper_process::try_start(&data_dir, &whisper_model, 9000).await
    } else {
        None
    };

    // Auto-start llamafile when --provider llamafile is requested.
    let _llamafile_guard = if provider == "llamafile" {
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
        llamafile_process::try_start(&data_dir, 8080).await
    } else {
        None
    };

    let session_id = "default-session".to_string();
    let agent = Arc::new(MockAgent::new());

    // Resolve the system prompt:
    // 1. If $DATA_DIR/prompts/system.md exists, load and render it with settings vars.
    // 2. Otherwise, build from settings values.
    // 3. Fall back to the static SYSTEM_PROMPT constant if settings are unavailable.
    let settings_repo = SqliteSettingsRepository::new(db.system.clone());
    let system_prompt = {
        let settings = settings_repo.get().await.ok();
        let prompt_dir = data_dir.join("prompts");
        let file_template = std::fs::read_to_string(prompt_dir.join("system.md")).ok();

        match (file_template, settings) {
            (Some(tmpl), Some(s)) => {
                println!("  Prompt:   custom ({})", prompt_dir.join("system.md").display());
                let name    = pond_core::prompts::sanitize_field(&s.assistant_name, 50);
                let user    = pond_core::prompts::sanitize_field(&s.user_name, 50);
                let persona = pond_core::prompts::sanitize_field(&s.assistant_personality, 200);
                let tz      = pond_core::prompts::sanitize_field(&s.timezone, 50);
                pond_core::prompts::render_template(&tmpl, &[
                    ("assistant_name", name.as_str()),
                    ("user_name",      user.as_str()),
                    ("personality",    persona.as_str()),
                    ("timezone",       tz.as_str()),
                ])
            }
            (None, Some(s)) => {
                println!(
                    "  Assistant: {} / greeting: {}",
                    s.assistant_name, s.user_name
                );
                build_system_prompt(&s.assistant_name, &s.user_name, &s.assistant_personality, &s.timezone)
            }
            _ => pond_core::prompts::SYSTEM_PROMPT.to_string(),
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

    // ── Wire LLM provider ──
    match provider {
        "llamafile" => {
            println!(
                "  Model:    {} (llamafile @ {})",
                pond_adapters_llamafile::DEFAULT_MODEL,
                pond_adapters_llamafile::DEFAULT_HOST
            );
            let llm = Arc::new(LlamafileProvider::new(None));
            chat_service = chat_service.with_provider(llm);
        }
        "ollama" => {
            let ollama_model = model.unwrap_or(pond_adapters_ollama::DEFAULT_MODEL);
            println!(
                "  Model:    {} (ollama @ {})",
                ollama_model,
                pond_adapters_ollama::DEFAULT_HOST
            );
            let llm = Arc::new(OllamaProvider::new(None, Some(ollama_model)));
            chat_service = chat_service.with_provider(llm);
        }
        _ => {
            println!("  Model:    mock (echo)");
        }
    }

    // ── Wire voice input ──
    let voice: Arc<dyn VoiceInput> = match input {
        "whisper" => {
            let url = whisper_url.unwrap_or(pond_adapters_whisper::DEFAULT_HOST);
            println!("  Input:    whisper (@ {})", url);
            Arc::new(WhisperInput::new(Some(url)))
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
        let trigger = wake_word.unwrap_or("goose");
        let url = whisper_url.unwrap_or(pond_adapters_whisper::DEFAULT_HOST);
        println!("  Wake word: \"{}\" (via whisper @ {})", trigger, url);
        Arc::new(WhisperKeywordDetector::new(Some(url), trigger))
    };
    chat_service = chat_service.with_wake_word_detector(detector);

    // Auto-start Qwen TTS server before the match (guard must outlive voice_out).
    let _qwen_tts_chat_guard = if tts == "qwen" || tts == "qwen-tts" {
        qwen_tts_process::try_start(pond_adapters_qwen_tts::DEFAULT_HOST, &data_dir).await
    } else {
        None
    };

    // ── Wire TTS output ──
    let voice_out: Arc<dyn VoiceOutput> = match tts {
        "qwen" | "qwen-tts" => {
            let qwen = Arc::new(QwenTtsOutput::new(None));

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
                run_chat("mock", None, "stdin", None, None, true, "none", None).await?;
            }
            "2" => {
                println!("Enter port (default 4000): ");
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let port: u16 = input.trim().parse().unwrap_or(4000);
                run_server(port, std::path::PathBuf::from("web/dist"), false).await?;
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

            Some(OnboardingStep::VerifyDevice) => {
                println!("Step: Verify Device");

                if let Some(ip) = get_local_ip() {
                    println!("Detected device IP: {}", ip);
                    user_data.insert("device_ip".to_string(), ip);
                } else {
                    println!("Could not detect IP, using default 'unknown'.");
                    user_data.insert("device_ip".to_string(), "unknown".to_string());
                }

                service.advance().await?;
            }

            Some(OnboardingStep::CreateProfile) => {
                println!("Step: Create Profile");

                let username = prompt_nonempty("Enter your username: ")?;
                user_data.insert("username".to_string(), username);

                service.advance().await?;
            }

            Some(OnboardingStep::ConfigurePersonality) => {
                println!("Step: Configure Personality");

                let personalities = vec![
                    "Friendly",
                    "Professional",
                    "Casual",
                    "Funny",
                    "Stoic",
                ];

                println!("Choose a personality for your assistant:");
                for (i, p) in personalities.iter().enumerate() {
                    println!("  {}) {}", i + 1, p);
                }

                let selected = loop {
                    let choice = prompt_nonempty("Enter the number of your choice: ")?;
                    if let Ok(index) = choice.parse::<usize>() {
                        if index >= 1 && index <= personalities.len() {
                            break personalities[index - 1].to_string();
                        }
                    }
                    println!("Invalid choice. Try again.");
                };

                println!("You selected: {}", selected);
                user_data.insert("personality".to_string(), selected);

                service.advance().await?;
            }

            Some(OnboardingStep::ConnectDevices) => {
                println!("Step: Connect Devices");
                println!("Press ENTER when all devices are connected...");
                let _ = io::stdin().read_line(&mut String::new())?;
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
