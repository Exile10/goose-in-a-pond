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

mod model_download;
mod whisper_process;

use anyhow::Result;
use clap::{Parser, Subcommand};
use pond_adapters_llamafile::LlamafileProvider;
use pond_adapters_ollama::OllamaProvider;
use pond_adapters_whisper::{WhisperInput, WhisperKeywordDetector};
use pond_core::ports::wake_word::WakeWordDetector;
use pond_core::services::instant_activation::InstantActivation;
use pond_api::AppState;
use pond_core::ports::agent::Agent;
use pond_core::ports::provider::LlmProvider;
use pond_core::ports::session_storage::SessionStorage;
use pond_core::ports::voice_input::VoiceInput;
use pond_core::services::chat::ChatService;
use pond_core::services::fallback_provider::FallbackProvider;
use pond_core::services::mock_agent::MockAgent;
use pond_core::services::stdin_input::StdinInput;
use pond_infra::db::Database;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
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
        Some(Commands::Chat { provider, model, input, whisper_url, wake_word, no_wake_word }) => {
            init_tracing(false);
            run_chat(&provider, model.as_deref(), &input, whisper_url.as_deref(), wake_word.as_deref(), no_wake_word).await
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
            run_chat("mock", None, "stdin", None, None, true).await
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

    // Step 1: Initialize databases
    println!("\n  [1/2] Initializing databases...");
    Database::init(&data_dir).await?;
    println!("  ✅ Databases ready");

    // Step 2: Download Whisper model
    let effective_model = if model.is_empty() {
        model_download::DEFAULT_WHISPER_MODEL
    } else {
        model
    };
    let expected_path = model_download::model_path(&data_dir, effective_model)?;
    println!("\n  [2/3] Downloading Whisper ASR model ({})...", effective_model);
    println!("  📁 Target: {}", expected_path.display());
    let model_path = model_download::download_whisper_model(effective_model, &data_dir).await?;

    // Step 3: Download whisper-server binary
    println!("\n  [3/3] Downloading whisper-server binary...");
    let _ = model_download::download_whisper_binary(&data_dir).await;

    // Print next steps
    let _ = model_path; // suppress unused warning

    println!();
    println!("  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  ✅ Setup complete!  Next steps:");
    println!();
    println!("  1. (Optional) Start llamafile for a local LLM:");
    println!("       ./your-model.llamafile");
    println!();
    println!("  2. Run the server (whisper.cpp starts automatically):");
    println!("       pond-server serve");
    println!();
    println!("  3. Or run interactive CLI chat with voice:");
    println!("       pond-server chat --provider llamafile --input whisper");
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

    // Auto-start whisper.cpp if not already running
    let whisper_model = model_download::model_path(&data_dir, model_download::DEFAULT_WHISPER_MODEL)
        .unwrap_or_else(|_| data_dir.join("models").join("ggml-base.en.bin"));
    let _whisper_guard = whisper_process::try_start(&data_dir, &whisper_model, 9000).await;

    // Build app state
    let onboarding_repo = Arc::new(SqlxOnboardingRepository::new(db.system.clone()));
    let session_storage: Arc<dyn pond_core::ports::session_storage::SessionStorage> =
        Arc::new(SqliteSessionStorage::new(db.system.clone()));

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

    // Resolve hostname
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "localhost".to_string());

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

async fn run_chat(provider: &str, model: Option<&str>, input: &str, whisper_url: Option<&str>, wake_word: Option<&str>, no_wake_word: bool) -> Result<()> {
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
        whisper_process::try_start(&data_dir, &whisper_model, 9000).await
    } else {
        None
    };

    let session_id = "default-session".to_string();
    let agent = Arc::new(MockAgent::new());
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

    let mut chat_service = ChatService::new(agent, session_id.clone(), storage);

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

    chat_service.run_loop().await?;

    Ok(())
}

async fn run_status() -> Result<()> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

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
                run_chat("mock", None, "stdin", None, None, true).await?;
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
