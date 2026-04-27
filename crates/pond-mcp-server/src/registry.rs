use std::sync::{Arc, OnceLock};
use pond_adapters_weather::WeatherProvider;
use pond_core::ports::device_registry::DeviceRegistry;
use pond_core::ports::memory_repository::MemoryRepository;
use pond_core::ports::recipe::AgentRecipeRepository;
use pond_core::ports::scheduler::SchedulerPort;
use pond_core::ports::settings::SettingsRepository;
use pond_core::ports::skill::UserSkillRepository;
use crate::giap_server::GiapMcpServer;
use rmcp::ServiceExt;
use tokio::io::DuplexStream;

pub struct GiapServiceHandles {
    pub weather:         Option<Arc<dyn WeatherProvider>>,
    pub device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    pub scheduler:       Option<Arc<dyn SchedulerPort>>,
    pub settings_repo:   Arc<dyn SettingsRepository + Send + Sync>,
    pub memory_repo:     Arc<dyn MemoryRepository + Send + Sync>,
    pub skill_repo:      Arc<dyn UserSkillRepository + Send + Sync>,
    pub recipe_repo:     Arc<dyn AgentRecipeRepository + Send + Sync>,
    /// Shared HTTP client for tools that fetch external data (Wikipedia, etc.).
    pub http_client:     reqwest::Client,
    /// The most recent user message, updated before each agent turn.
    /// Used as fallback when a tool is called with empty parameters
    /// (common with small local models that struggle with tool-call schemas).
    pub last_user_message: tokio::sync::RwLock<String>,
}

static GIAP_SERVICES: OnceLock<Arc<GiapServiceHandles>> = OnceLock::new();

pub fn init_giap_services(services: Arc<GiapServiceHandles>) {
    let _ = GIAP_SERVICES.set(services);
}

/// Update the last user message so tools can use it as a fallback when the
/// model calls a tool with empty parameters.
pub async fn set_last_user_message(msg: &str) {
    if let Some(handles) = GIAP_SERVICES.get() {
        *handles.last_user_message.write().await = msg.to_string();
    }
}

pub fn spawn_giap_server(reader: DuplexStream, writer: DuplexStream) {
    let services = GIAP_SERVICES
        .get()
        .expect("GIAP services not initialized — call init_giap_services() first")
        .clone();
    let server = GiapMcpServer::new(services);
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => {
                tracing::error!("GIAP MCP server failed to start: {e}");
            }
        }
    });
}
