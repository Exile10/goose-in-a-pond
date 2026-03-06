//! Goose In A Pond — Server Entry Point
//!
//! Usage:
//!   pond-server serve [--port PORT] [--open]
//!   pond-server chat  [--provider mock|ollama]
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
//!
//! # TODO — CLI commands
//! - [ ] `serve`  — Start HTTP server + REST API + web dashboard
//! - [ ] `chat`   — Interactive CLI chat (Wait→Listen→Think→Speak loop)
//! - [ ] `status` — Show system info (hostname, port, DB status, onboarding state)
//! - [ ] `onboard` — Start onboarding wizard in terminal
//! - [ ] `debug`  — Show debug info, tail logs
//! - [ ] Open browser automatically if host has a display (headful mode)

use anyhow::Result;
use clap::{Parser, Subcommand};
use pond_api::AppState;
use pond_core::services::chat::ChatService;
use pond_core::services::mock_agent::MockAgent;
use pond_infra::db::Database;
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
    /// Start the HTTP server (REST API + web dashboard)
    Serve {
        /// Port to listen on (default: 4000)
        #[arg(short, long, default_value = "4000")]
        port: u16,

        /// Open the dashboard in the browser
        #[arg(long)]
        open: bool,

        /// Enable debug logging
        #[arg(long)]
        debug: bool,
    },

    /// Interactive CLI chat (Wait→Listen→Think→Speak loop)
    Chat {
        /// LLM provider: mock or ollama
        #[arg(short = 'P', long, default_value = "mock")]
        provider: String,
    },

    /// Show system status
    Status,

    /// Run onboarding
    Onboard,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Serve { port, open, debug }) => {
            init_tracing(debug);
            run_server(port, open).await
        }
        Some(Commands::Chat { provider }) => {
            init_tracing(false);
            run_chat(&provider).await
        }
        Some(Commands::Status) => {
            run_status().await
        }
        Some(Commands::Onboard) => run_onboard().await,
        None => {
            // Default: run interactive chat (backward compat)
            init_tracing(false);
            run_chat("mock").await
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

async fn run_server(port: u16, open: bool) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose In A Pond  v{}       ║", env!("CARGO_PKG_VERSION"));
    println!("  ╚═══════════════════════════════════════╝");

    // Initialize databases
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("goose-in-a-pond");
    let db = Database::init(&data_dir).await?;

    // Build app state
    let state = Arc::new(AppState {
        db: Arc::new(db),
    });

    // Build router
    let app = pond_api::build_router(state);

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

async fn run_chat(provider: &str) -> Result<()> {
    println!("  ╔═══════════════════════════════════════╗");
    println!("  ║   🦆  Goose-in-a-Pond  v{}       ║", env!("CARGO_PKG_VERSION"));
    println!("  ║   Wait → Listen → Think → Speak      ║");
    println!("  ╚═══════════════════════════════════════╝");
    println!("  Provider: {}", provider);

    // TODO: match on provider to select MockAgent, OllamaProvider, etc.
    let agent = Arc::new(MockAgent::new());
    let chat_service = ChatService::new(agent, "default-session".to_string());
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

pub (crate) fn get_local_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.168.1.1:80").ok()?;
    let local_addr = socket.local_addr().ok()?;
    Some(local_addr.ip().to_string())
}

pub (crate) fn prompt_nonempty(prompt: &str) -> Result<String> {
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
pub async fn run_onboard() -> Result<()> {
    println!("🦆 Goose In A Pond — Interactive Onboarding Wizard\n");

    // Initialize database
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("goose-in-a-pond");
    let db = Database::init(&data_dir).await?;
    let repo = SqlxOnboardingRepository::new(db.system.clone());
    let service = OnboardingService::new(repo);

    // persist user_data once UserProfile domain + port exist
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
                println!("\n Onboarding complete! Here’s your info:\n");

                for (key, value) in &user_data {
                    println!("  {}: {}", key, value);
                }

                break;
            }
        }
    }

    Ok(())
}
